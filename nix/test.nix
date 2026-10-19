{ pkgs, self }:
pkgs.testers.runNixOSTest {
  name = "floppa-service";
  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    services.floppa-vpn = {
      enable = true;
      users = [ "alice" ];
      autostart = false;
    };
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
    };
    users.users.bob = {
      isNormalUser = true;
      uid = 1001;
    };
    # No interactive desktop in this VM; grant only the packaged helper action.
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id == "dev.okhsunrog.floppa-vpn.network-config" && subject.user == "alice")
          return polkit.Result.YES;
      });
    '';
    environment.etc."floppa-test.conf".text = ''
      [Interface]
      PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
      Address = 10.77.0.2/24
      [Peer]
      PublicKey = AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
      Endpoint = 127.0.0.1:9
      AllowedIPs = 10.77.0.0/24
    '';
    environment.systemPackages = [
      pkgs.libcap
      pkgs.python3
    ];
  };
  testScript = ''
    start_all()
    machine.wait_for_unit("floppa-vpn.socket")
    machine.succeed("test $(stat -c %a /run/floppa-vpn/vpn.sock) = 660")
    machine.succeed("test $(stat -c %G /run/floppa-vpn/vpn.sock) = floppa")
    machine.succeed("su - alice -c '/run/wrappers/bin/floppa status'")
    machine.wait_for_unit("floppa-vpn.service")
    machine.fail("su - bob -c '/run/wrappers/bin/floppa status'")
    machine.succeed("getcap /run/wrappers/bin/floppa | grep cap_net_admin")
    # A standalone process must never try to install into /usr on NixOS.
    machine.succeed("test -x ${self.packages.x86_64-linux.cli.resources}/lib/floppa-vpn/floppa-network-helper")
    machine.succeed("systemctl stop floppa-vpn.socket floppa-vpn.service")
    machine.succeed("su - alice -c '/run/wrappers/bin/floppa status' 2>&1 | grep -i standalone")
    # Exercise the real standalone helper and external UDP socket. No provider,
    # enrollment, internet endpoint or production credentials are involved.
    machine.succeed("su - alice -c '/run/wrappers/bin/floppa connect --config /etc/floppa-test.conf --interface floppa0 --no-dns' > /tmp/standalone.log 2>&1 &")
    machine.wait_until_succeeds("ip link show floppa0", timeout=30)
    machine.wait_until_succeeds("ss -uanep | grep 'fwmark:0x666c6f70'", timeout=30)
    machine.succeed("grep -i standalone /tmp/standalone.log")
    machine.succeed("pkill -INT -u alice -f 'floppa.* connect' || true")
  '';
}
