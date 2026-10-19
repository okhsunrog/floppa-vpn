{ craneLib, cargoLock }:
craneLib.vendorCargoDeps {
  inherit cargoLock;
  # Foldfin's vendored h3 crates retain publishing backups. Cargo rejects
  # Cargo.toml.orig when packaging a Git checkout; it is not build input.
  overrideVendorGitCheckout =
    _: drv:
    drv.overrideAttrs (old: {
      postPatch = (old.postPatch or "") + "find . -name Cargo.toml.orig -type f -delete";
    });
}
