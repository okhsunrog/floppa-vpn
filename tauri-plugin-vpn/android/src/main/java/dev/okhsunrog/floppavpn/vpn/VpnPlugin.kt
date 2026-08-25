package dev.okhsunrog.floppavpn.vpn

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.BitmapDrawable
import android.net.Uri
import android.net.VpnService
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.util.Base64
import android.util.Log
import android.webkit.WebView
import androidx.activity.result.ActivityResult
import androidx.core.app.NotificationManagerCompat
import androidx.core.graphics.createBitmap
import androidx.core.graphics.scale
import androidx.core.net.toUri
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream

@InvokeArg
class VpnConfigArgs {
    /** Required; the service refuses a start intent without it rather than inventing one. */
    var ipv4Addr: String? = null
    var ipv6Addr: String? = null
    var routes: Array<String> = emptyArray()
    var dns: String? = null
    var mtu: Int = 1280
    var disallowedApps: Array<String> = emptyArray()
    var allowedApps: Array<String> = emptyArray()
    /** Generation of the request, echoed back so a superseded service instance is rejectable. */
    var epoch: Long = 0
}

@InvokeArg
class StatusBarStyleArgs {
    var isDark: Boolean = false
}

@TauriPlugin(
    permissions =
        [Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "postNotification")]
)
class VpnPlugin(private val activity: Activity) : Plugin(activity) {

    companion object {
        private const val NOTIFICATION_ALIAS = "postNotification"
    }

    override fun load(webView: WebView) {
        // In the two-process architecture, FloppaVpnService runs in :vpn process.
        // No eventCallback setup needed — the UI process communicates via tarpc.
    }

    @Command
    fun prepareVpn(invoke: Invoke) {
        val intent = VpnService.prepare(activity)
        val ret = JSObject()

        if (intent != null) {
            // Need to request permission — use Tauri's activity result API
            startActivityForResult(invoke, intent, "vpnPermissionResult")
        } else {
            // Already have permission
            ret.put("granted", true)
            invoke.resolve(ret)
        }
    }

    @ActivityCallback
    fun vpnPermissionResult(invoke: Invoke, result: ActivityResult) {
        val granted = result.resultCode == Activity.RESULT_OK
        Log.d("VpnPlugin", "vpnPermissionResult: resultCode=${result.resultCode}, granted=$granted")
        val ret = JSObject()
        ret.put("granted", granted)
        invoke.resolve(ret)
    }

    @Command
    fun startVpn(invoke: Invoke) {
        try {
            Log.d("VpnPlugin", "startVpn called")
            val args = invoke.parseArgs(VpnConfigArgs::class.java)
            Log.d(
                "VpnPlugin",
                "startVpn args parsed: ipv4=${args.ipv4Addr}, routes=${args.routes.joinToString()}, dns=${args.dns}, mtu=${args.mtu}",
            )

            val ipv4Addr = args.ipv4Addr
            if (ipv4Addr.isNullOrEmpty()) {
                invoke.reject("ipv4Addr is required")
                return
            }

            // Check if VPN is prepared
            val prepareIntent = VpnService.prepare(activity)
            if (prepareIntent != null) {
                invoke.reject("VPN permission not granted. Call prepareVpn first.")
                return
            }

            // Stop any existing VPN service
            activity.stopService(Intent(activity, FloppaVpnService::class.java))

            // Start the VPN service in :vpn process
            val intent =
                Intent(activity, FloppaVpnService::class.java).apply {
                    putExtra(FloppaVpnService.EXTRA_IPV4_ADDR, ipv4Addr)
                    putExtra(FloppaVpnService.EXTRA_IPV6_ADDR, args.ipv6Addr)
                    putExtra(FloppaVpnService.EXTRA_ROUTES, args.routes)
                    putExtra(FloppaVpnService.EXTRA_DNS, args.dns)
                    putExtra(FloppaVpnService.EXTRA_MTU, args.mtu)
                    putExtra(FloppaVpnService.EXTRA_DISALLOWED_APPS, args.disallowedApps)
                    putExtra(FloppaVpnService.EXTRA_ALLOWED_APPS, args.allowedApps)
                    putExtra(FloppaVpnService.EXTRA_EPOCH, args.epoch)
                }

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                activity.startForegroundService(intent)
            } else {
                activity.startService(intent)
            }
            Log.d("VpnPlugin", "VPN service started in :vpn process")
            invoke.resolve()
        } catch (e: Exception) {
            Log.e("VpnPlugin", "startVpn error", e)
            invoke.reject("Failed to start VPN: ${e.message}")
        }
    }

    /**
     * Stop the service out of band.
     *
     * The normal stop is the RPC `stop`, after which the service stops itself. This path exists for
     * the instance the RPC cannot reach: a bind that failed, a socket file that went missing.
     * ACTION_STOP is delivered to onStartCommand of whichever instance is alive and runs the same
     * shutdown sequence there (nativeStop, cleanup, stopSelf). `startService` is refused while the
     * app is in the background (API 26+), so `stopService` — the plain API, and what `startVpn`
     * uses to clear a previous instance — is the fallback. Rejects only if both are refused.
     */
    @Command
    fun stopVpn(invoke: Invoke) {
        val stopIntent =
            Intent(activity, FloppaVpnService::class.java).apply {
                action = FloppaVpnService.ACTION_STOP
            }
        try {
            activity.startService(stopIntent)
            Log.i("VpnPlugin", "stopVpn: sent ACTION_STOP intent")
            invoke.resolve()
            return
        } catch (e: Exception) {
            Log.w("VpnPlugin", "stopVpn: ACTION_STOP refused, falling back to stopService", e)
        }
        try {
            val wasRunning = activity.stopService(Intent(activity, FloppaVpnService::class.java))
            Log.i("VpnPlugin", "stopVpn: stopService called (was running: $wasRunning)")
            invoke.resolve()
        } catch (e: Exception) {
            Log.e("VpnPlugin", "stopVpn: stopService failed", e)
            invoke.reject("Failed to stop VPN service: ${e.message}")
        }
    }

    /**
     * Get list of installed apps for split tunneling UI.
     *
     * Returns non-system apps with their package names and display labels. The own app is excluded
     * from the list.
     */
    @Command
    fun getInstalledApps(invoke: Invoke) {
        // Run on background thread to avoid blocking the Android UI thread. Anything thrown on it
        // would otherwise kill the process with no reply ever reaching the caller.
        Thread {
            try {
                invoke.resolve(collectInstalledApps())
            } catch (e: Exception) {
                Log.e("VpnPlugin", "getInstalledApps error", e)
                invoke.reject("Failed to list installed apps: ${e.message}")
            }
        }
            .start()
    }

    private fun collectInstalledApps(): JSObject {
        val pm = activity.packageManager
        val apps =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                pm.getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))
            } else {
                @Suppress("DEPRECATION") pm.getInstalledApplications(0)
            }

        // One query for every launcher entry, rather than one getLaunchIntentForPackage binder
        // call per installed package.
        val launcherIntent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        val launchable =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                pm.queryIntentActivities(launcherIntent, PackageManager.ResolveInfoFlags.of(0))
            } else {
                @Suppress("DEPRECATION") pm.queryIntentActivities(launcherIntent, 0)
            }
        val launchablePackages = launchable.map { it.activityInfo.packageName }.toHashSet()

        val ownPackage = activity.packageName
        val result = JSObject()
        val appList = JSArray()
        val iconSize = (32 * activity.resources.displayMetrics.density).toInt()

        for (appInfo in apps) {
            if (appInfo.packageName == ownPackage) continue

            // Preinstalled apps carry FLAG_SYSTEM, but user-facing ones (YouTube, Maps,
            // Chrome, …) have a launcher entry. Treat those as non-system so they show up
            // in the main list instead of being hidden behind "show system apps".
            val isSystemFlag = (appInfo.flags and ApplicationInfo.FLAG_SYSTEM) != 0
            val hasLauncher = appInfo.packageName in launchablePackages
            val isSystem = isSystemFlag && !hasLauncher

            val entry = JSObject()
            entry.put("packageName", appInfo.packageName)
            entry.put("label", appInfo.loadLabel(pm).toString())
            entry.put("isSystem", isSystem)

            try {
                val drawable = appInfo.loadIcon(pm)
                val bitmap =
                    if (drawable is BitmapDrawable) {
                        drawable.bitmap.scale(iconSize, iconSize)
                    } else {
                        val bmp = createBitmap(iconSize, iconSize)
                        val canvas = Canvas(bmp)
                        drawable.setBounds(0, 0, iconSize, iconSize)
                        drawable.draw(canvas)
                        bmp
                    }
                val stream = ByteArrayOutputStream()
                bitmap.compress(Bitmap.CompressFormat.PNG, 80, stream)
                entry.put("icon", Base64.encodeToString(stream.toByteArray(), Base64.NO_WRAP))
            } catch (_: Exception) {
                // Icon loading failed, leave null
            }

            appList.put(entry)
        }

        result.put("apps", appList)
        return result
    }

    /**
     * Get safe area insets (status bar, navigation bar) in density-independent pixels.
     *
     * Goes through the compat API so the values are real on every supported release. The platform
     * `WindowInsets.getInsets(Type)` only exists from API 30, and the previous version answered 0/0
     * below that — which, with edge-to-edge enabled, put the content under the status bar on
     * Android 7–10.
     */
    @Command
    fun getSafeAreaInsets(invoke: Invoke) {
        val ret = JSObject()
        val insets = ViewCompat.getRootWindowInsets(activity.window.decorView)
        if (insets != null) {
            val density = activity.resources.displayMetrics.density
            val bars =
                insets.getInsets(
                    WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
                )
            ret.put("top", (bars.top / density).toDouble())
            ret.put("bottom", (bars.bottom / density).toDouble())
        } else {
            ret.put("top", 0)
            ret.put("bottom", 0)
        }
        invoke.resolve(ret)
    }

    /**
     * Set status bar style to match app theme. isDark=true → light icons (for dark backgrounds)
     * isDark=false → dark icons (for light backgrounds)
     */
    @Command
    fun setStatusBarStyle(invoke: Invoke) {
        val args = invoke.parseArgs(StatusBarStyleArgs::class.java)
        activity.runOnUiThread {
            WindowCompat.getInsetsController(activity.window, activity.window.decorView)
                .isAppearanceLightStatusBars = !args.isDark
        }
        invoke.resolve()
    }

    /**
     * Get a stable device ID that persists across app reinstalls.
     *
     * ANDROID_ID deliberately: a peer record on the server is keyed to a device, and an identifier
     * that changed on reinstall would orphan the peer and consume a second slot from the user's
     * limit. It is per-app-signing-key and not a hardware serial, so it identifies this
     * installation rather than the handset.
     */
    @SuppressLint("HardwareIds")
    @Command
    fun getDeviceId(invoke: Invoke) {
        val androidId =
            Settings.Secure.getString(activity.contentResolver, Settings.Secure.ANDROID_ID)
        val ret = JSObject()
        ret.put("id", androidId)
        invoke.resolve(ret)
    }

    /** Get device name (manufacturer + model) for peer identification. */
    @Command
    fun getDeviceName(invoke: Invoke) {
        val manufacturer = Build.MANUFACTURER.replaceFirstChar { it.uppercase() }
        val model = Build.MODEL
        // If model already starts with manufacturer, don't duplicate
        val name =
            if (model.startsWith(manufacturer, ignoreCase = true)) {
                model
            } else {
                "$manufacturer $model"
            }
        val ret = JSObject()
        ret.put("name", name)
        invoke.resolve(ret)
    }

    /** Check if the app is excluded from battery optimization. */
    @Command
    fun isBatteryOptimizationDisabled(invoke: Invoke) {
        val pm = activity.getSystemService(Activity.POWER_SERVICE) as PowerManager
        val ret = JSObject()
        ret.put("disabled", pm.isIgnoringBatteryOptimizations(activity.packageName))
        invoke.resolve(ret)
    }

    /**
     * Request the user to disable battery optimization for this app. Shows a direct system dialog
     * asking to allow unrestricted background usage. Resolves with { "disabled": true/false } after
     * the user responds.
     */
    @SuppressLint("BatteryLife")
    @Command
    fun requestDisableBatteryOptimization(invoke: Invoke) {
        try {
            val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
            intent.data = "package:${activity.packageName}".toUri()
            startActivityForResult(invoke, intent, "batteryOptimizationResult")
        } catch (e: Exception) {
            Log.e("VpnPlugin", "requestDisableBatteryOptimization error", e)
            invoke.reject("Failed to open battery settings: ${e.message}")
        }
    }

    @ActivityCallback
    fun batteryOptimizationResult(invoke: Invoke, result: ActivityResult) {
        val pm = activity.getSystemService(Activity.POWER_SERVICE) as PowerManager
        val disabled = pm.isIgnoringBatteryOptimizations(activity.packageName)
        Log.d(
            "VpnPlugin",
            "batteryOptimizationResult: resultCode=${result.resultCode}, disabled=$disabled",
        )
        val ret = JSObject()
        ret.put("disabled", disabled)
        invoke.resolve(ret)
    }

    /** Check if notifications are enabled for this app. */
    @Command
    fun areNotificationsEnabled(invoke: Invoke) {
        val ret = JSObject()
        ret.put("enabled", NotificationManagerCompat.from(activity).areNotificationsEnabled())
        invoke.resolve(ret)
    }

    /**
     * Request notification permission. On Android 13+ shows a runtime permission dialog via Tauri's
     * permission system. On older versions opens the app's notification settings page and resolves
     * when the user comes back from it — resolving right after `startActivity`, as this used to,
     * reported the state from before the user had touched anything. Resolves with { "enabled":
     * true/false }.
     */
    @Command
    fun openNotificationSettings(invoke: Invoke) {
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                requestPermissionForAlias(
                    NOTIFICATION_ALIAS,
                    invoke,
                    "notificationPermissionCallback",
                )
            } else {
                // ACTION_APP_NOTIFICATION_SETTINGS only exists from API 26; below that the app
                // details page is where notifications are toggled.
                val intent =
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                        Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).apply {
                            putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
                        }
                    } else {
                        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                            data = Uri.fromParts("package", activity.packageName, null)
                        }
                    }
                startActivityForResult(invoke, intent, "notificationSettingsResult")
            }
        } catch (e: Exception) {
            Log.e("VpnPlugin", "openNotificationSettings error", e)
            invoke.reject("Failed to request notification permission: ${e.message}")
        }
    }

    @ActivityCallback
    fun notificationSettingsResult(invoke: Invoke, result: ActivityResult) {
        val enabled = NotificationManagerCompat.from(activity).areNotificationsEnabled()
        Log.d(
            "VpnPlugin",
            "notificationSettingsResult: resultCode=${result.resultCode}, enabled=$enabled",
        )
        val ret = JSObject()
        ret.put("enabled", enabled)
        invoke.resolve(ret)
    }

    @PermissionCallback
    fun notificationPermissionCallback(invoke: Invoke) {
        val enabled = NotificationManagerCompat.from(activity).areNotificationsEnabled()
        Log.d("VpnPlugin", "notificationPermissionCallback: enabled=$enabled")
        val ret = JSObject()
        ret.put("enabled", enabled)
        invoke.resolve(ret)
    }
}
