//! Durable write-ahead records. The directory and lock inode are shared with v1.
use crate::validate::{boot, iface};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

pub const STATE: &str = "/var/lib/floppa-vpn/network-rollback-v1";
const LIMIT: u64 = 1_048_576;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    record: Record,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Record {
    Policy {
        boot: String,
        ipv4: bool,
        ipv6: bool,
        old_mark: Option<u8>,
        token: Option<String>,
    },
    Routes {
        boot: String,
        iface: String,
        index: Option<u32>,
        destinations: Option<Vec<String>>,
    },
    Link {
        boot: String,
        iface: String,
        temporary: Option<String>,
        token: Option<String>,
        uid: Option<u32>,
    },
    Resolved {
        boot: String,
        iface: String,
        index: Option<u32>,
    },
    FileDns {
        original: Resolver,
        installed: Option<Fingerprint>,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Resolver {
    Absent,
    Link { target: PathBuf },
    File { bytes: Vec<u8> },
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub device: u64,
    pub inode: u64,
    pub sha256: String,
}

pub struct Journal {
    dir: PathBuf,
    lock: File,
}
pub fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("sync {}", path.display()))
}
pub fn secure_dir(path: &Path, mode: u32) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_dir() && m.uid() == 0 && m.mode() & 0o022 == 0,
            "unsafe directory {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(mode).create(path)?;
            sync_dir(path.parent().context("directory parent")?)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub fn secure_read(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
    };
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == 0 && m.nlink() == 1 && m.mode() & 0o077 == 0 && m.len() <= LIMIT,
        "unsafe recovery record {}",
        path.display()
    );
    let mut data = Vec::new();
    (&mut f).take(LIMIT + 1).read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= LIMIT, "oversized recovery record");
    Ok(Some(data))
}
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("file parent")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    crate::checkpoint("file-synced");
    tmp.persist(path).map_err(|e| e.error)?;
    crate::checkpoint("file-renamed");
    sync_dir(parent)?;
    crate::checkpoint("directory-synced");
    Ok(())
}
impl Journal {
    pub fn open() -> Result<Self> {
        secure_dir(Path::new("/var/lib/floppa-vpn"), 0o700)?;
        let dir = PathBuf::from(STATE);
        secure_dir(&dir, 0o700)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("lock"))?;
        let m = lock.metadata()?;
        ensure!(
            m.is_file() && m.uid() == 0 && m.nlink() == 1 && m.mode() & 0o077 == 0,
            "unsafe helper lock"
        );
        // SAFETY: valid open descriptor, no borrowed memory.
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0,
            "lock recovery journal: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self { dir, lock })
    }
    pub fn fd(&self) -> RawFd {
        self.lock.as_raw_fd()
    }
    pub fn save(&self, name: &str, record: Record) -> Result<()> {
        ensure!(
            fs::symlink_metadata(self.dir.join(name))
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "pending {name} recovery; recover the previous tunnel first"
        );
        let bytes = serde_json::to_vec(&Envelope { version: 2, record })?;
        ensure!(bytes.len() as u64 <= LIMIT, "oversized journal");
        atomic_write(&self.dir.join(name), &bytes, 0o600)
    }
    pub fn load(&self, name: &str) -> Result<Option<Record>> {
        let Some(data) = secure_read(&self.dir.join(name))? else {
            return Ok(None);
        };
        let record = if data.first() == Some(&b'{') {
            let envelope: Envelope =
                serde_json::from_slice(&data).context("invalid recovery JSON")?;
            ensure!(
                envelope.version == 2,
                "unsupported journal version {}",
                envelope.version
            );
            envelope.record
        } else {
            self.legacy(name, &data)?
        };
        record.validate(name)?;
        Ok(Some(record))
    }
    pub fn clear(&self, name: &str) -> Result<()> {
        match fs::remove_file(self.dir.join(name)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        sync_dir(&self.dir)?;
        if name == "dns" {
            match fs::remove_file(self.dir.join("resolver-backup")) {
                Ok(()) => sync_dir(&self.dir)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn legacy(&self, name: &str, data: &[u8]) -> Result<Record> {
        ensure!(data.last() == Some(&0), "truncated legacy {name} record");
        let fields = data[..data.len() - 1]
            .split(|b| *b == 0)
            .map(std::str::from_utf8)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let record = match (name, fields.as_slice()) {
            ("policy", [v4, v6, mark, id]) => Record::Policy {
                boot: id.to_string(),
                ipv4: flag(v4)?,
                ipv6: flag(v6)?,
                old_mark: match *mark {
                    "-" => None,
                    "0" => Some(0),
                    "1" => Some(1),
                    _ => bail!("invalid saved mark"),
                },
                token: None,
            },
            ("routes", [name, id]) => Record::Routes {
                boot: id.to_string(),
                iface: name.to_string(),
                index: None,
                destinations: None,
            },
            ("link", [name, id]) => Record::Link {
                boot: id.to_string(),
                iface: name.to_string(),
                temporary: None,
                token: None,
                uid: None,
            },
            ("dns", ["resolved", name, id]) => Record::Resolved {
                boot: id.to_string(),
                iface: name.to_string(),
                index: None,
            },
            ("dns", fields) => {
                let (kind, rest) = fields.split_first().context("empty DNS record")?;
                let (kind, installed, rest) = if let Some(kind) = kind.strip_suffix("-v2") {
                    ensure!(rest.len() >= 2, "invalid DNS fingerprint");
                    let (original, fp) = rest.split_at(rest.len() - 2);
                    let (dev, ino) = fp[0].split_once(':').context("invalid inode identity")?;
                    (
                        kind,
                        Some(Fingerprint {
                            device: dev.parse()?,
                            inode: ino.parse()?,
                            sha256: fp[1].to_string(),
                        }),
                        original,
                    )
                } else {
                    (*kind, None, rest)
                };
                let original = match (kind, rest) {
                    ("absent", []) => Resolver::Absent,
                    ("link", [target]) if !target.is_empty() => Resolver::Link {
                        target: PathBuf::from(target),
                    },
                    ("file", []) => Resolver::File {
                        bytes: secure_read(&self.dir.join("resolver-backup"))?
                            .context("missing resolver backup")?,
                    },
                    _ => bail!("invalid legacy DNS record"),
                };
                Record::FileDns {
                    original,
                    installed,
                }
            }
            _ => bail!("invalid legacy {name} record"),
        };
        Ok(record)
    }
}
fn flag(s: &str) -> Result<bool> {
    match s {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => bail!("invalid boolean"),
    }
}
impl Record {
    fn validate(&self, name: &str) -> Result<()> {
        match self {
            Self::Policy {
                boot: id,
                old_mark,
                token,
                ..
            } => {
                ensure!(name == "policy", "wrong record kind");
                boot(id)?;
                ensure!(old_mark.is_none_or(|x| x <= 1), "invalid mark");
                if let Some(t) = token {
                    boot(t)?;
                }
            }
            Self::Routes {
                boot: id,
                iface: i,
                destinations,
                ..
            } => {
                ensure!(name == "routes", "wrong record kind");
                boot(id)?;
                iface(i)?;
                if let Some(d) = destinations {
                    for n in d {
                        crate::validate::network(n)?;
                    }
                }
            }
            Self::Link {
                boot: id,
                iface: i,
                token,
                temporary,
                uid,
            } => {
                ensure!(name == "link", "wrong record kind");
                boot(id)?;
                iface(i)?;
                if let Some(t) = token {
                    boot(t)?;
                    ensure!(
                        temporary.as_deref() == Some(&format!("flpt{}", &t[..8])) && uid.is_some(),
                        "invalid link identity"
                    );
                } else {
                    ensure!(
                        temporary.is_none() && uid.is_none(),
                        "incomplete link identity"
                    );
                }
            }
            Self::Resolved {
                boot: id, iface: i, ..
            } => {
                ensure!(name == "dns", "wrong record kind");
                boot(id)?;
                iface(i)?;
            }
            Self::FileDns {
                original,
                installed,
            } => {
                ensure!(name == "dns", "wrong record kind");
                if let Some(fp) = installed {
                    ensure!(
                        fp.sha256.len() == 64 && fp.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                        "invalid DNS digest"
                    );
                }
                if let Resolver::Link { target } = original {
                    ensure!(!target.as_os_str().is_empty(), "empty resolver link");
                }
            }
        }
        Ok(())
    }
}
