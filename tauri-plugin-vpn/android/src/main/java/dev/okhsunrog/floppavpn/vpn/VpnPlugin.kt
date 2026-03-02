package dev.okhsunrog.floppavpn.vpn

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
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream

@InvokeArg
class VpnConfigArgs {
    var ipv4Addr: String = "10.0.0.2/24"
    var ipv6Addr: String? = null
    var routes: Array<String> = emptyArray()
    var dns: String? = null
    var mtu: Int = 1280
    var disallowedApps: Array<String> = emptyArray()
    var allowedApps: Array<String> = emptyArray()
    /** Raw WireGuard config string, passed to :vpn process via Intent */
    var wgConfig: String? = null
}

@InvokeArg
class ProtectSocketArgs {
    var fd: Int = -1
}

@TauriPlugin
class VpnPlugin(private val activity: Activity) : Plugin(activity) {

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
            Log.d("VpnPlugin", "startVpn args parsed: ipv4=${args.ipv4Addr}, routes=${args.routes.joinToString()}, dns=${args.dns}, mtu=${args.mtu}")

            if (args.wgConfig == null) {
                invoke.reject("Missing wgConfig parameter")
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
            val intent = Intent(activity, FloppaVpnService::class.java).apply {
                putExtra(FloppaVpnService.EXTRA_IPV4_ADDR, args.ipv4Addr)
                putExtra(FloppaVpnService.EXTRA_IPV6_ADDR, args.ipv6Addr)
                putExtra(FloppaVpnService.EXTRA_ROUTES, args.routes)
                putExtra(FloppaVpnService.EXTRA_DNS, args.dns)
                putExtra(FloppaVpnService.EXTRA_MTU, args.mtu)
                putExtra(FloppaVpnService.EXTRA_DISALLOWED_APPS, args.disallowedApps)
                putExtra(FloppaVpnService.EXTRA_ALLOWED_APPS, args.allowedApps)
                putExtra(FloppaVpnService.EXTRA_WG_CONFIG, args.wgConfig)
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

    @Command
    fun stopVpn(invoke: Invoke) {
        // Send stopService intent to :vpn process — Android delivers it cross-process
        activity.stopService(Intent(activity, FloppaVpnService::class.java))
        invoke.resolve()
    }

    @Command
    fun getVpnStatus(invoke: Invoke) {
        // In the two-process architecture, we can't check FloppaVpnService.instance
        // from the UI process. The UI queries status via tarpc through Rust commands.
        // This command returns "unknown" — the TS side should use getConnectionInfo() instead.
        val ret = JSObject()
        ret.put("status", "unknown")
        invoke.resolve(ret)
    }

    /**
     * Get list of installed apps for split tunneling UI.
     *
     * Returns non-system apps with their package names and display labels.
     * The own app is excluded from the list.
     */
    @Command
    fun getInstalledApps(invoke: Invoke) {
        val pm = activity.packageManager
        val apps = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))
        } else {
            @Suppress("DEPRECATION")
            pm.getInstalledApplications(0)
        }

        val ownPackage = activity.packageName
        val result = JSObject()
        val appList = JSArray()
        val iconSize = (32 * activity.resources.displayMetrics.density).toInt()

        for (appInfo in apps) {
            // Skip own app
            if (appInfo.packageName == ownPackage) continue

            // Skip system apps without a launcher icon (background services, etc.)
            val isSystem = (appInfo.flags and ApplicationInfo.FLAG_SYSTEM) != 0
            val hasLauncherIntent = pm.getLaunchIntentForPackage(appInfo.packageName) != null
            if (isSystem && !hasLauncherIntent) continue

            val entry = JSObject()
            entry.put("packageName", appInfo.packageName)
            entry.put("label", appInfo.loadLabel(pm).toString())
            entry.put("isSystem", isSystem)

            // Load app icon as base64 PNG
            try {
                val drawable = appInfo.loadIcon(pm)
                val bitmap = if (drawable is BitmapDrawable) {
                    Bitmap.createScaledBitmap(drawable.bitmap, iconSize, iconSize, true)
                } else {
                    val bmp = Bitmap.createBitmap(iconSize, iconSize, Bitmap.Config.ARGB_8888)
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
        invoke.resolve(result)
    }

    /**
     * Get safe area insets (status bar, navigation bar) in density-independent pixels.
     */
    @Command
    fun getSafeAreaInsets(invoke: Invoke) {
        val ret = JSObject()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val insets = activity.window.decorView.rootWindowInsets
            if (insets != null) {
                val density = activity.resources.displayMetrics.density
                val statusBars = insets.getInsets(android.view.WindowInsets.Type.statusBars())
                val navBars = insets.getInsets(android.view.WindowInsets.Type.navigationBars())
                val cutout = insets.getInsets(android.view.WindowInsets.Type.displayCutout())
                // Top inset = max of status bar and display cutout
                val topPx = maxOf(statusBars.top, cutout.top)
                val bottomPx = navBars.bottom
                ret.put("top", (topPx / density).toDouble())
                ret.put("bottom", (bottomPx / density).toDouble())
            } else {
                ret.put("top", 0)
                ret.put("bottom", 0)
            }
        } else {
            ret.put("top", 0)
            ret.put("bottom", 0)
        }
        invoke.resolve(ret)
    }

    /**
     * Get device name (manufacturer + model) for peer identification.
     */
    @Command
    fun getDeviceName(invoke: Invoke) {
        val manufacturer = Build.MANUFACTURER.replaceFirstChar { it.uppercase() }
        val model = Build.MODEL
        // If model already starts with manufacturer, don't duplicate
        val name = if (model.startsWith(manufacturer, ignoreCase = true)) {
            model
        } else {
            "$manufacturer $model"
        }
        val ret = JSObject()
        ret.put("name", name)
        invoke.resolve(ret)
    }

    /**
     * Check if the app is excluded from battery optimization.
     */
    @Command
    fun isBatteryOptimizationDisabled(invoke: Invoke) {
        val pm = activity.getSystemService(Activity.POWER_SERVICE) as PowerManager
        val ret = JSObject()
        ret.put("disabled", pm.isIgnoringBatteryOptimizations(activity.packageName))
        invoke.resolve(ret)
    }

    /**
     * Request the user to disable battery optimization for this app.
     */
    @Command
    fun requestDisableBatteryOptimization(invoke: Invoke) {
        try {
            val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                data = Uri.parse("package:${activity.packageName}")
            }
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            Log.e("VpnPlugin", "requestDisableBatteryOptimization error", e)
            invoke.reject("Failed to request battery optimization: ${e.message}")
        }
    }

    /**
     * Check if notifications are enabled for this app.
     */
    @Command
    fun areNotificationsEnabled(invoke: Invoke) {
        val ret = JSObject()
        ret.put("enabled", NotificationManagerCompat.from(activity).areNotificationsEnabled())
        invoke.resolve(ret)
    }

    /**
     * Open the app's notification settings.
     */
    @Command
    fun openNotificationSettings(invoke: Invoke) {
        try {
            val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).apply {
                putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
            }
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            Log.e("VpnPlugin", "openNotificationSettings error", e)
            invoke.reject("Failed to open notification settings: ${e.message}")
        }
    }

    /**
     * Protect a socket from VPN routing.
     *
     * Note: In the two-process architecture, this is primarily used by Rust JNI
     * in the :vpn process (calling FloppaVpnService.protectSocket directly).
     * This Tauri command is kept for backwards compatibility but may not work
     * cross-process.
     */
    @Command
    fun protectSocket(invoke: Invoke) {
        val args = invoke.parseArgs(ProtectSocketArgs::class.java)
        val ret = JSObject()
        // This won't work cross-process since FloppaVpnService.instance is per-process.
        // Socket protection is handled locally in the :vpn process via JNI.
        ret.put("protected", false)
        invoke.resolve(ret)
    }
}
