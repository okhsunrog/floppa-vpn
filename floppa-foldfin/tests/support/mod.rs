pub fn certificate() -> (Vec<u8>, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    assert!(
        std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-days",
                "1",
                "-keyout"
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .args([
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-addext",
                "extendedKeyUsage=serverAuth"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    (std::fs::read(cert).unwrap(), std::fs::read(key).unwrap())
}
