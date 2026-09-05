// Read or change one app's Onyx "App Optimization" (EAC) config on a Boox, from `adb shell`.
//
// Why this exists: `OECService` in the Onyx framework kills every running process of every app
// whose EAC config says `enable && supportEAC` the moment the launcher switches EAC on after boot,
// about 2.5 s after BOOT_COMPLETED. An always-on VPN started by the system is up by then, and dies.
// The per-app switch in the EinkWise panel ("Master Switch") writes through KSync into the *theme*
// store, which the boot cull does not read, and on firmware 4.2 with the Onyx cloud unreachable
// nothing reached the store it does read. This tool writes to that store directly, through the
// same binder method the launcher is supposed to reach: `IOECService.applyAppConfigToService`.
//
// Two fields matter:
//   enable        — the boot cull skips the app when false ("Disable App Optimization")
//   fullPMAccess  — `EACPMImpl.initAppForceStandBy` sets appop RUN_ANY_IN_BACKGROUND=ignore at
//                   every boot for apps where this is false ("Stay Active in the Background")
//
// Runs under `app_process` as the shell user, which may `find` the `oec_service` binder
// (`dumpsys oec_service` works from a shell; `dumpsys vpn` does not). Built and run by
// `just boox-eac`. Nothing here is specific to this app: pass any package name.
//
// Verified on a NoteAir4C, Android 13, firmware 2026-04-28_17-50_4.2: with enable=false and
// fullPMAccess=true the always-on tunnel survived boot, and the config survived the reboot.

import android.os.Bundle;
import org.json.JSONObject;
import java.lang.reflect.Method;
import java.util.Collections;
import java.util.List;

public class OecTool {
    public static void main(String[] args) throws Throwable {
        if (args.length < 2) {
            System.out.println("usage: OecTool get <pkg>");
            System.out.println("       OecTool set <pkg> [enable=true|false] [fullPMAccess=true|false]");
            System.exit(2);
        }
        String mode = args[0];
        String pkg = args[1];
        if (!mode.equals("get") && !mode.equals("set")) {
            System.out.println("unknown mode: " + mode);
            System.exit(2);
        }

        // android.onyx.optimization.EInkHelper is a framework class on Onyx firmware only, hence
        // reflection: this file compiles against a stock android.jar.
        Class<?> helper = Class.forName("android.onyx.optimization.EInkHelper");
        Object svc = helper.getMethod("getService").invoke(null);
        Method get = svc.getClass().getMethod("getAppConfigFromService", List.class);
        Method apply = svc.getClass().getMethod("applyAppConfigToService", List.class, Bundle.class);

        JSONObject cfg = read(get, svc, pkg);
        if (cfg == null) {
            System.out.println("no EAC config for " + pkg + " (never launched on this device?)");
            System.exit(1);
        }
        System.out.println("before: " + summary(cfg));
        if (mode.equals("get")) return;

        for (int i = 2; i < args.length; i++) {
            String[] kv = args[i].split("=", 2);
            if (kv.length != 2 || !(kv[1].equals("true") || kv[1].equals("false"))) {
                System.out.println("expected key=true|false, got: " + args[i]);
                System.exit(2);
            }
            boolean value = Boolean.parseBoolean(kv[1]);
            if (kv[0].equals("enable")) {
                cfg.put("enable", value);
            } else if (kv[0].equals("fullPMAccess")) {
                cfg.getJSONObject("extraConfig").put("fullPMAccess", value);
            } else {
                System.out.println("unknown key: " + kv[0]);
                System.exit(2);
            }
        }

        // The same extras the launcher passes; args_save_mmkv makes the service persist at once.
        Bundle params = new Bundle();
        params.putInt("args_operation_flag", 0);
        params.putBoolean("args_save_mmkv", true);
        apply.invoke(svc, Collections.singletonList(cfg.toString()), params);

        // The apply runs on the service's own scheduler; give it a moment before reading back.
        // Note that getAppConfigFromService answers from the *theme* store, so the read-back can
        // lag the truth; `adb shell dumpsys oec_service | grep <pkg>` prints the store that counts.
        Thread.sleep(1500);
        JSONObject after = read(get, svc, pkg);
        System.out.println("after:  " + (after == null ? "(gone?)" : summary(after)));
        System.out.println("check:  adb shell dumpsys oec_service | grep " + pkg);
    }

    private static JSONObject read(Method get, Object svc, String pkg) throws Throwable {
        @SuppressWarnings("unchecked")
        List<String> cfgs = (List<String>) get.invoke(svc, Collections.singletonList(pkg));
        if (cfgs == null || cfgs.isEmpty()) return null;
        return new JSONObject(cfgs.get(0));
    }

    private static String summary(JSONObject cfg) {
        JSONObject extra = cfg.optJSONObject("extraConfig");
        return "enable=" + cfg.opt("enable")
            + " supportEAC=" + cfg.opt("supportEAC")
            + " fullPMAccess=" + (extra == null ? "?" : extra.opt("fullPMAccess"))
            + " fullPMAccessTimeout=" + (extra == null ? "?" : extra.opt("fullPMAccessTimeout"));
    }
}
