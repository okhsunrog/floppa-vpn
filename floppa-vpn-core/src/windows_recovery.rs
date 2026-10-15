//! Machine-scoped rollback storage. Only elevated administrators and SYSTEM may edit it.
//! Never read an elevated recovery journal from a per-user directory.
use std::{io, path::PathBuf};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Com::CoTaskMemFree,
    UI::Shell::*,
};
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(Some(0)).collect()
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "shared recovery directory must be owned by Administrators/SYSTEM with a protected admin-only ACL, without reparse points",
    )
}
unsafe fn trusted_sid(sid: PSID) -> bool {
    !sid.is_null()
        && unsafe {
            IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
                || IsWellKnownSid(sid, WinLocalSystemSid) != 0
        }
}
pub(crate) fn journal_path() -> io::Result<PathBuf> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        let result = SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, std::ptr::null_mut(), &mut raw);
        if result < 0 {
            return Err(io::Error::other(format!(
                "ProgramData lookup failed: {result:#x}"
            )));
        }
        let mut length = 0;
        while *raw.add(length) != 0 {
            length += 1;
        }
        use std::os::windows::ffi::OsStringExt;
        let base = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
            raw, length,
        )));
        CoTaskMemFree(raw.cast());
        let dir = base.join("FloppaVPN-Recovery-v1");
        let path = wide(dir.as_os_str());
        let sddl = wide(std::ffi::OsStr::new(
            "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
        ));
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let created = CreateDirectoryW(path.as_ptr(), &attrs);
        let error = GetLastError();
        LocalFree(descriptor);
        if created == 0 && error != ERROR_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let attributes = GetFileAttributesW(path.as_ptr());
        if attributes == INVALID_FILE_ATTRIBUTES
            || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || attributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err(denied());
        }
        let mut owner = std::ptr::null_mut();
        let mut acl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let code = GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        let result = (|| {
            let mut control = 0;
            let mut revision = 0;
            if !trusted_sid(owner)
                || acl.is_null()
                || GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0
                || control & SE_DACL_PROTECTED == 0
            {
                return Err(denied());
            }
            if (*acl).AceCount == 0 {
                return Err(denied());
            }
            for i in 0..(*acl).AceCount {
                let mut ace = std::ptr::null_mut();
                if GetAce(acl, u32::from(i), &mut ace) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                // ACCESS_ALLOWED_ACE_TYPE = 0; reject unfamiliar/object/callback ACEs.
                if ace.Header.AceType != 0
                    || !trusted_sid((&ace.SidStart as *const u32).cast_mut().cast())
                {
                    return Err(denied());
                }
            }
            Ok(dir.join("rollback.json"))
        })();
        LocalFree(descriptor);
        result
    }
}
