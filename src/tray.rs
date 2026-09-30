use crate::client::translate;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use crate::ipc::Data;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use hbb_common::tokio;
use hbb_common::{allow_err, log};
use base::config::keys;
use std::sync::{Arc, Mutex};
#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::time::Duration;

/// AUMID (Application User Model ID) for incoming-notify toasts. Windows
/// labels every toast with the identity behind this ID; the PowerShell AUMID
/// the toast crate falls back to would make notifications show up as coming
/// from "Windows PowerShell". Registering our own AUMID per user (see
/// `ensure_toast_identity`) attributes the toast to the app instead.
#[cfg(windows)]
const TOAST_AUMID: &str = "RustDesk.IncomingNotify";

pub fn start_tray() {
    if crate::ui_interface::get_builtin_option(keys::OPTION_HIDE_TRAY) == "Y" {
        #[cfg(not(target_os = "macos"))]
        {
            return;
        }
    }

    #[cfg(target_os = "linux")]
    crate::server::check_zombie();

    allow_err!(make_tray());
}

fn make_tray() -> hbb_common::ResultType<()> {
    // https://github.com/tauri-apps/tray-icon/blob/dev/examples/tao.rs
    use hbb_common::anyhow::Context;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        TrayIcon, TrayIconBuilder, TrayIconEvent as TrayEvent,
    };

    // Duplicated tray icons kept piling up through the blind spots of
    // `check_process("--tray", ..)`. https://github.com/rustdesk/rustdesk/issues/15689
    #[cfg(windows)]
    if !crate::platform::windows::try_lock_tray_single_instance() {
        log::info!("Another tray process is already running in this session, exit");
        return Ok(());
    }

    let icon;
    #[cfg(target_os = "macos")]
    {
        icon = include_bytes!("../res/mac-tray-dark-x2.png"); // use as template, so color is not important
    }
    #[cfg(not(target_os = "macos"))]
    {
        icon = include_bytes!("../res/tray-icon.ico");
    }

    let (icon_rgba, icon_width, icon_height) = {
        let image = load_icon_from_asset()
            .unwrap_or(image::load_from_memory(icon).context("Failed to open icon path")?)
            .into_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        (rgba, width, height)
    };
    let icon = tray_icon::Icon::from_rgba(icon_rgba, icon_width, icon_height)
        .context("Failed to open icon")?;

    let mut event_loop = EventLoopBuilder::new().build();

    let tray_menu = Menu::new();
    let hide_stop_service = crate::ui_interface::get_builtin_option(
        keys::OPTION_HIDE_STOP_SERVICE,
    ) == "Y";
    // The tray icon is only shown when the service is running, so we don't need to check
    // the `stop-service` option here.
    let quit_i = if !hide_stop_service {
        Some(MenuItem::new(translate("Stop service".to_owned()), true, None))
    } else {
        None
    };
    let open_i = MenuItem::new(translate("Open".to_owned()), true, None);
    if let Some(quit_i) = &quit_i {
        tray_menu.append_items(&[&open_i, quit_i]).ok();
    } else {
        tray_menu.append_items(&[&open_i]).ok();
    }
    let tooltip = |count: usize| {
        if count == 0 {
            format!(
                "{} {}",
                crate::get_app_name(),
                translate("Service is running".to_owned()),
            )
        } else {
            format!(
                "{} - {}\n{}",
                crate::get_app_name(),
                translate("Ready".to_owned()),
                translate("{".to_string() + &format!("{count}") + "} sessions"),
            )
        }
    };
    let mut _tray_icon: Arc<Mutex<Option<TrayIcon>>> = Default::default();

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayEvent::receiver();
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let (ipc_sender, ipc_receiver) = std::sync::mpsc::channel::<Data>();

    let open_func = move || {
        if cfg!(not(feature = "flutter")) {
            crate::run_me::<&str>(vec![]).ok();
            return;
        }
        #[cfg(target_os = "macos")]
        crate::platform::macos::handle_application_should_open_untitled_file();
        #[cfg(target_os = "windows")]
        {
            // Do not use "start uni link" way, it may not work on some Windows, and pop out error
            // dialog, I found on one user's desktop, but no idea why, Windows is shit.
            // Use `run_me` instead.
            // `allow_multiple_instances` in `flutter/windows/runner/main.cpp` allows only one instance without args.
            crate::run_me::<&str>(vec![]).ok();
        }
        #[cfg(target_os = "linux")]
        {
            // Do not use "xdg-open", it won't read the config.
            if crate::dbus::invoke_new_connection(crate::get_uri_prefix()).is_err() {
                if let Ok(task) = crate::run_me::<&str>(vec![]) {
                    crate::server::CHILD_PROCESS.lock().unwrap().push(task);
                }
            }
        }
    };

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    std::thread::spawn(move || {
        start_query_session_count(ipc_sender.clone());
    });
    #[cfg(windows)]
    let mut last_click = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::EventLoopExtMacOS;
        event_loop.set_activation_policy(tao::platform::macos::ActivationPolicy::Accessory);
    }
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(100),
        );

        if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
            // for fixing https://github.com/rustdesk/rustdesk/discussions/10210#discussioncomment-14600745
            // so we start tray, but not to show it
            if crate::ui_interface::get_builtin_option(keys::OPTION_HIDE_TRAY) == "Y" {
                return;
            }
            // We create the icon once the event loop is actually running
            // to prevent issues like https://github.com/tauri-apps/tray-icon/issues/90
            let mut builder = TrayIconBuilder::new()
                .with_id(crate::get_app_name().to_lowercase())
                .with_menu(Box::new(tray_menu.clone()))
                .with_tooltip(tooltip(0))
                .with_icon(icon.clone());
            #[cfg(target_os = "macos")]
            {
                builder = builder.with_icon_as_template(true);
            }
            #[cfg(target_os = "windows")]
            {
                // Required since tray-icon 0.17
                // Fixes #15215, #15222, #15410
                builder = builder.with_menu_on_left_click(false);
            }
            let tray = builder.build();
            match tray {
                Ok(tray) => _tray_icon = Arc::new(Mutex::new(Some(tray))),
                Err(err) => {
                    log::error!("Failed to create tray icon: {}", err);
                }
            };

            // We have to request a redraw here to have the icon actually show up.
            // Tao only exposes a redraw method on the Window so we use core-foundation directly.
            #[cfg(target_os = "macos")]
            unsafe {
                use core_foundation::runloop::{CFRunLoopGetMain, CFRunLoopWakeUp};

                let rl = CFRunLoopGetMain();
                CFRunLoopWakeUp(rl);
            }
        }

        if let Ok(event) = menu_channel.try_recv() {
            if let Some(quit_i) = &quit_i {
                if event.id == quit_i.id() {
                    /* failed in windows, seems no permission to check system process
                    if !crate::check_process("--server", false) {
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    */
                    // Remove the icon first: on success `uninstall_service()` ends
                    // this process with `std::process::exit`, which skips the
                    // destructor that would remove it, leaving a ghost icon behind.
                    #[cfg(windows)]
                    let _ = _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_visible(false));
                    if !crate::platform::uninstall_service(false, false) {
                        *control_flow = ControlFlow::Exit;
                    }
                    // Still alive, so stopping the service failed or was cancelled
                    // in the UAC prompt. Show the icon again.
                    #[cfg(windows)]
                    let _ = _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_visible(true));
                } else if event.id == open_i.id() {
                    open_func();
                }
            } else if event.id == open_i.id() {
                open_func();
            }
        }

        if let Ok(_event) = tray_channel.try_recv() {
            #[cfg(target_os = "windows")]
            match _event {
                TrayEvent::Click {
                    button,
                    button_state,
                    ..
                } => {
                    if button == tray_icon::MouseButton::Left
                        && button_state == tray_icon::MouseButtonState::Up
                    {
                        if last_click.elapsed() < std::time::Duration::from_secs(1) {
                            return;
                        }
                        open_func();
                        last_click = std::time::Instant::now();
                    }
                }
                _ => {}
            }
        }

        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Ok(data) = ipc_receiver.try_recv() {
            match data {
                Data::ControlledSessionCount(count) => {
                    _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_tooltip(Some(tooltip(count))));
                }
                Data::PeerIncomingNotify(name) => {
                    let text = format!(
                        "{} {}",
                        name,
                        translate("is controlling this device".to_string())
                    );
                    #[cfg(windows)]
                    {
                        // Aliased import: `Duration` is already taken by
                        // std::time::Duration at the top of this file.
                        use tauri_winrt_notification::{
                            Duration as ToastDuration, Sound, Toast,
                        };
                        // Windows Server and IoT editions have no reliable toast
                        // notification support: Toast::show() either errors or
                        // silently no-ops on these SKUs, which looks exactly like
                        // the feature not firing. Go straight to the Shell balloon
                        // tip there; on standard client editions keep the nicer
                        // toast and fall back to the balloon when the shell
                        // refuses it (app notifications disabled, Focus Assist,
                        // unknown AUMID, ...).
                        if !should_use_toast_notification() {
                            log::info!(
                                "incoming notify: server/IoT SKU, using balloon tip for {:?}",
                                name
                            );
                            show_balloon_tip(&crate::get_app_name(), &text);
                        } else {
                            // Surface the result instead of swallowing it: a Toast
                            // that the shell refuses would otherwise look exactly
                            // like the feature not firing at all.
                            // Attribute the toast to RustDesk rather than to
                            // "Windows PowerShell": the label Windows shows comes
                            // from the AUMID registry entry, which we re-register
                            // here (idempotent, HKCU, no admin needed) right
                            // before every toast.
                            ensure_toast_identity();
                            match Toast::new(TOAST_AUMID)
                                .title(&crate::get_app_name())
                                .text1(&text)
                                .sound(Some(Sound::Default))
                                .duration(ToastDuration::Short)
                                .show()
                            {
                                Ok(()) => log::info!("incoming notify: toast shown for {:?}", name),
                                Err(e) => {
                                    log::warn!(
                                        "incoming notify: toast failed for {:?}: {}, falling back to balloon tip",
                                        name, e
                                    );
                                    show_balloon_tip(&crate::get_app_name(), &text);
                                }
                            }
                        }
                    }
                    #[cfg(target_os = "linux")]
                    {
                        show_desktop_notify(&crate::get_app_name(), &text, &name);
                    }
                }
                _ => {}
            }
        }
    });
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[tokio::main(flavor = "current_thread")]
async fn start_query_session_count(sender: std::sync::mpsc::Sender<Data>) {
    let mut last_count = 0;
    // Peer ids seen on the previous poll, so we only notify about peers that
    // just started controlling this machine.
    let mut last_peers: Vec<String> = Vec::new();
    loop {
        if let Ok(mut c) = crate::ipc::connect(1000, "").await {
            let mut timer = crate::rustdesk_interval(tokio::time::interval(Duration::from_secs(1)));
            loop {
                tokio::select! {
                    res = c.next() => {
                        match res {
                            Err(err) => {
                                log::error!("ipc connection closed: {}", err);
                                break;
                            }

                            Ok(Some(Data::ControlledSessionCount(count))) => {
                                if count != last_count {
                                    last_count = count;
                                    sender.send(Data::ControlledSessionCount(count)).ok();
                                }
                            }
                            Ok(Some(Data::ControlledSessionDetail(list))) => {
                                let peer_ids: Vec<String> =
                                    list.iter().map(|(id, _, _, _)| id.clone()).collect();
                                // Notify only about peers that were not
                                // connected on the previous poll. Disconnects
                                // are intentionally silent. The switch is
                                // machine-level (Config), matching the fact
                                // that the service is the one that knows.
                                if hbb_common::config::Config::get_bool_option(
                                    keys::OPTION_ALLOW_INCOMING_NOTIFY,
                                ) {
                                    for (peer_id, peer_name, _, _) in list.iter() {
                                        if !last_peers.contains(peer_id) {
                                            sender
                                                .send(Data::PeerIncomingNotify(peer_name.clone()))
                                                .ok();
                                        }
                                    }
                                }
                                last_peers = peer_ids;
                            }
                            _ => {}
                        }
                    }

                    _ = timer.tick() => {
                        c.send(&Data::ControlledSessionCount(0)).await.ok();
                        c.send(&Data::ControlledSessionDetail(vec![])).await.ok();
                    }
                }
            }
        }
        hbb_common::sleep(1.).await;
    }
}

/// True on Windows Server editions (InstallationType contains "Server").
/// Server SKUs have no toast notification infrastructure, so the tray falls
/// back to a Shell balloon tip there. Unknown registry state reports
/// "not a server" and keeps the toast path.
#[cfg(windows)]
fn is_windows_server() -> bool {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    let Ok(key) = winreg::RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion")
    else {
        return false;
    };
    key.get_value::<String, _>("InstallationType")
        .map(|t| t.contains("Server"))
        .unwrap_or(false)
}

/// True when the current Windows SKU should use WinRT Toast notifications.
/// Returns false for Server editions (no toast infrastructure) and IoT
/// editions (WinRT runtime may be incomplete or restricted), directing
/// those SKUs straight to the more reliable Shell balloon tip path.
#[cfg(windows)]
fn should_use_toast_notification() -> bool {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    // Server editions have no toast support at all
    if is_windows_server() {
        return false;
    }
    // Detect IoT editions (LTSC / Enterprise IoT / IoT Enterprise) —
    // these report as "Client" in InstallationType but often lack the
    // full WinRT runtime needed for Toast::show() to work reliably.
    if let Ok(key) = winreg::RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion")
    {
        if let Ok(product_name) = key.get_value::<String, _>("ProductName") {
            if product_name.contains("IoT") {
                log::info!(
                    "incoming notify: detected IoT edition ({}), using balloon tip",
                    product_name
                );
                return false;
            }
        }
    }
    true  // Default: allow Toast on standard client Windows
}

/// Registers `TOAST_AUMID` under `HKCU\SOFTWARE\Classes\AppUserModelId` with
/// a friendly `DisplayName` — the classic way for unpackaged desktop apps to
/// own their toast identity (no admin rights, no MSIX, no shortcut tricks).
/// Idempotent: once the value exists, re-running it on every toast is a
/// cheap no-op. Failure here is not fatal: the toast then shows with an
/// unregistered identity and the existing balloon-tip fallback takes over
/// if the shell refuses it.
#[cfg(windows)]
fn ensure_toast_identity() {
    use winreg::enums::HKEY_CURRENT_USER;
    let path = format!("SOFTWARE\\Classes\\AppUserModelId\\{}", TOAST_AUMID);
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.create_subkey(&path) {
        Ok((key, _)) => {
            if let Err(e) = key.set_value("DisplayName", &crate::get_app_name()) {
                log::warn!("incoming notify: failed to set toast DisplayName: {}", e);
            }
        }
        Err(e) => {
            log::warn!("incoming notify: failed to register toast AUMID: {}", e);
        }
    }
}

/// Check whether the current Windows session has an active Explorer shell
/// (i.e. a taskbar / notification area exists). Server Core installations
/// and some IoT configurations run without Explorer.
#[cfg(windows)]
fn has_desktop_shell() -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq explorer.exe", "/NH"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("explorer.exe"))
        .unwrap_or(false)
}

/// Balloon tip (the classic Shell_NotifyIcon `NIF_INFO` balloon) delivered
/// through a transient PowerShell `NotifyIcon`. This is the notification path
/// that still works on Windows Server, where WinRT toasts are unavailable.
/// Runs detached so a slow PowerShell start can never stall the tray event
/// loop; the helper process removes its own icon after 12 seconds.
///
/// Identity: on Win10/11 the Shell converts balloons to toasts and, when the
/// owning process has no explicit AUMID, it generates one whose DisplayName
/// comes from the process — for powershell.exe that is "Windows PowerShell"
/// (our v4 toast fix did not cover this fallback path). So the helper sets
/// `TOAST_AUMID` as its process AUMID before creating the NotifyIcon, makes
/// sure the AUMID registry entry (DisplayName = app name) exists, and sets
/// the NotifyIcon tooltip to the app name as an extra hint. If any of that
/// fails, the balloon still goes out, only mislabeled as before.
///
/// On Server Core (no Explorer shell) this function logs a warning and
/// returns immediately: there is no notification area for a balloon to
/// appear in, and launching PowerShell would only produce a misleading
/// "balloon tip shown" log entry.
#[cfg(windows)]
fn show_balloon_tip(title: &str, text: &str) {
    // No Explorer shell = no notification area = nowhere for the balloon to appear.
    // Skip the PowerShell launch entirely rather than logging a misleading "shown".
    if !has_desktop_shell() {
        log::warn!(
            "incoming notify: no desktop shell (explorer.exe not running), \
             skipping balloon tip on this session"
        );
        return;
    }
    let esc = |s: &str| s.replace('\'', "''");
    let script = format!(
        "try {{ Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
         public static class Aumid {{ [DllImport(\"shell32.dll\", PreserveSig=false)] \
         public static extern void SetCurrentProcessExplicitAppUserModelID([MarshalAs(UnmanagedType.LPWStr)] string id); }}'; \
         }} catch {{ }}; \
         Add-Type -AssemblyName System.Windows.Forms; \
         try {{ New-Item -Path 'HKCU:\\SOFTWARE\\Classes\\AppUserModelId\\{aumid}' -Force | Out-Null; \
         Set-ItemProperty -Path 'HKCU:\\SOFTWARE\\Classes\\AppUserModelId\\{aumid}' -Name 'DisplayName' -Value '{app}'; \
         }} catch {{ }}; \
         try {{ [Aumid]::SetCurrentProcessExplicitAppUserModelID('{aumid}'); }} catch {{ }}; \
         $n = New-Object System.Windows.Forms.NotifyIcon; \
         $n.Icon = [System.Drawing.SystemIcons]::Information; \
         $n.Text = '{app}'; \
         $n.Visible = $true; \
         $n.ShowBalloonTip(10000, '{title}', '{text}', [System.Windows.Forms.ToolTipIcon]::Info); \
         Start-Sleep -Seconds 12; \
         $n.Dispose();",
        aumid = TOAST_AUMID,
        app = esc(&crate::get_app_name()),
        title = esc(title),
        text = esc(text)
    );
    std::thread::spawn(move || {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        match std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(out) if out.status.success() => {
                log::info!("incoming notify: balloon tip shown");
            }
            Ok(out) => {
                log::warn!(
                    "incoming notify: balloon tip failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => {
                log::warn!("incoming notify: balloon tip failed to launch: {}", e);
            }
        }
    });
}

/// Linux desktop notification through `notify-send`, the libnotify CLI that
/// ships with every mainstream desktop. `-a` sets the source label, so the
/// bubble shows up as coming from RustDesk instead of from the process name.
/// Runs detached so a slow DBus activation can never stall the tray event
/// loop. No new dependency, no unsafe code: if the tray process has no
/// `DBUS_SESSION_BUS_ADDRESS` (headless / minimal install) the command simply
/// fails and the warning is logged — same policy as on Windows, where a
/// refused notification is logged instead of swallowed silently.
#[cfg(target_os = "linux")]
fn show_desktop_notify(title: &str, text: &str, name: &str) {
    let app = crate::get_app_name();
    let title = title.to_owned();
    let text = text.to_owned();
    let name = name.to_owned();
    std::thread::spawn(move || {
        let args = [
            "-a",
            app.as_str(),
            "-t",
            "10000",
            "--icon",
            "rustdesk",
            title.as_str(),
            text.as_str(),
        ];
        match std::process::Command::new("notify-send").args(args).output() {
            Ok(out) if out.status.success() => {
                log::info!("incoming notify: desktop notification shown for {:?}", name);
            }
            Ok(out) => {
                log::warn!(
                    "incoming notify: notify-send failed for {:?}: {} (no DBUS_SESSION_BUS_ADDRESS or no desktop session)",
                    name,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => {
                log::warn!(
                    "incoming notify: notify-send is unavailable for {:?}: {}",
                    name,
                    e
                );
            }
        }
    });
}

fn load_icon_from_asset() -> Option<image::DynamicImage> {
    let Some(path) = std::env::current_exe().map_or(None, |x| x.parent().map(|x| x.to_path_buf()))
    else {
        return None;
    };
    #[cfg(target_os = "macos")]
    let path = path.join("../Frameworks/App.framework/Resources/flutter_assets/assets/icon.png");
    #[cfg(windows)]
    let path = path.join(r"data\flutter_assets\assets\icon.png");
    #[cfg(target_os = "linux")]
    let path = path.join(r"data/flutter_assets/assets/icon.png");
    if path.exists() {
        if let Ok(image) = image::open(path) {
            return Some(image);
        }
    }
    None
}
