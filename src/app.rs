use crate::config::{edit_config_file, Config};
use crate::foreground::ForegroundWatcher;
use crate::keyboard::KeyboardListener;
use crate::painter::{AppsPainter, Overlay, WindowsPainter};
use crate::startup::Startup;
use crate::trayicon::TrayIcon;
use crate::utils::{
    check_error, get_app_icon, get_foreground_window, get_window_user_data, is_iconic_window,
    is_running_as_admin, list_windows, set_foreground_window, set_window_user_data,
};

use anyhow::{anyhow, Result};
use indexmap::IndexSet;
use std::collections::HashMap;
use windows::core::{w, PCWSTR};
use windows::Win32::{
    Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyIcon, DispatchMessageW, GetMessageW, LoadCursorW,
        PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, TranslateMessage,
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HICON, HTCLIENT, IDC_ARROW, MSG, WM_COMMAND,
        WM_ERASEBKGND, WM_LBUTTONUP, WM_NCHITTEST, WM_RBUTTONUP, WNDCLASSW, WS_EX_NOACTIVATE,
        WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    },
};

pub const NAME: PCWSTR = w!("Window Switcher");
pub const WM_USER_TRAYICON: u32 = 6000;
pub const WM_USER_REGISTER_TRAYICON: u32 = 6001;
pub const WM_USER_SWITCH_APPS: u32 = 6010;
pub const WM_USER_SWITCH_APPS_DONE: u32 = 6011;
pub const WM_USER_SWITCH_APPS_CANCEL: u32 = 6012;
pub const WM_USER_SWITCH_WINDOWS: u32 = 6020;
pub const WM_USER_SWITCH_WINDOWS_DONE: u32 = 6021;
pub const WM_USER_SWITCH_APP_WINDOWS: u32 = 6030;
pub const WM_USER_SWITCH_APP_WINDOWS_DONE: u32 = 6031;
pub const WM_USER_SWITCH_APP_WINDOWS_CANCEL: u32 = 6032;
pub const IDM_EXIT: u32 = 1;
pub const IDM_STARTUP: u32 = 2;
pub const IDM_CONFIGURE: u32 = 3;

pub fn start(config: &Config) -> Result<()> {
    info!("start config={config:?}");
    App::start(config)
}

/// Listen to this message to recreate the tray icon since the taskbar has been recreated.
static mut WM_TASKBARCREATED: u32 = 0;

pub struct App {
    hwnd: HWND,
    is_admin: bool,
    trayicon: Option<TrayIcon>,
    startup: Startup,
    config: Config,
    switch_windows_state: SwitchWindowsState,
    switch_apps_state: Option<SwitchAppsState>,
    switch_app_windows_state: Option<SwitchAppWindowsState>,
    cached_icons: HashMap<String, HICON>,
    apps_painter: AppsPainter,
    // `windows_painter` is declared before `overlay` on purpose: it holds GDI+ font objects
    // that must be freed (its Drop) before the overlay shuts GDI+ down (overlay's Drop).
    windows_painter: WindowsPainter,
    overlay: Overlay,
}

impl App {
    pub fn start(config: &Config) -> Result<()> {
        let hwnd = Self::create_window()?;
        let overlay = Overlay::new(hwnd, config.switch_apps_colors)?;
        let windows_painter = WindowsPainter::new();

        let _foreground_watcher = ForegroundWatcher::init(&config.switch_windows_blacklist)?;
        let _keyboard_listener = KeyboardListener::init(hwnd, &config.to_hotkeys())?;

        let trayicon = match config.trayicon {
            true => Some(TrayIcon::create()),
            false => None,
        };

        let is_admin = is_running_as_admin()?;
        debug!("is_admin {is_admin}");

        let startup = Startup::init(is_admin)?;

        let mut app = App {
            hwnd,
            is_admin,
            trayicon,
            startup,
            config: config.clone(),
            switch_windows_state: SwitchWindowsState {
                cache: None,
                modifier_released: true,
            },
            switch_apps_state: None,
            switch_app_windows_state: None,
            cached_icons: Default::default(),
            apps_painter: AppsPainter::new(),
            windows_painter,
            overlay,
        };

        app.set_trayicon();

        let app_ptr = Box::into_raw(Box::new(app)) as _;
        check_error(|| set_window_user_data(hwnd, app_ptr))
            .map_err(|err| anyhow!("Failed to set window ptr, {err}"))?;

        Self::eventloop()
    }

    fn eventloop() -> Result<()> {
        let mut message = MSG::default();
        loop {
            let ret = unsafe { GetMessageW(&mut message, None, 0, 0) };
            match ret.0 {
                -1 => {
                    unsafe { GetLastError() }.ok()?;
                }
                0 => break,
                _ => unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                },
            }
        }

        Ok(())
    }

    fn create_window() -> Result<HWND> {
        unsafe { WM_TASKBARCREATED = RegisterWindowMessageW(w!("TaskbarCreated")) };

        let hinstance = unsafe { GetModuleHandleW(None) }
            .map_err(|err| anyhow!("Failed to get current module handle, {err}"))?;

        let hcursor = unsafe { LoadCursorW(None, IDC_ARROW) }
            .map_err(|err| anyhow!("Failed to load arrow cursor, {err}"))?;

        let window_class = WNDCLASSW {
            hCursor: hcursor,
            hInstance: HINSTANCE(hinstance.0),
            lpszClassName: NAME,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(App::window_proc),
            ..Default::default()
        };

        let atom = check_error(|| unsafe { RegisterClassW(&window_class) })
            .map_err(|err| anyhow!("Failed to register class, {err}"))?;

        let hwnd = unsafe {
            CreateWindowExW(
                // A composition-hosted window: no GDI redirection surface (the compositor
                // draws every pixel), topmost, off the taskbar, and never activates.
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                PCWSTR(atom as _),
                NAME,
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .map_err(|err| anyhow!("Failed to create windows, {err}"))?;

        Ok(hwnd)
    }

    fn set_trayicon(&mut self) {
        if let Some(trayicon) = self.trayicon.as_mut() {
            match trayicon.register(self.hwnd) {
                Ok(()) => info!("trayicon registered"),
                Err(err) => {
                    if !trayicon.exist() {
                        error!("{err}, retrying in 3 second");
                        let hwnd = self.hwnd.0 as isize;
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs(3));
                            let _ = unsafe {
                                PostMessageW(
                                    Some(HWND(hwnd as _)),
                                    WM_USER_REGISTER_TRAYICON,
                                    WPARAM(0),
                                    LPARAM(0),
                                )
                            };
                        });
                    }
                }
            }
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match Self::handle_message(hwnd, msg, wparam, lparam) {
            Ok(ret) => ret,
            Err(err) => {
                error!("{err}");
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }

    fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Result<LRESULT> {
        match msg {
            WM_USER_TRAYICON => {
                let app = get_app(hwnd)?;
                if let Some(trayicon) = app.trayicon.as_mut() {
                    let keycode = lparam.0 as u32;
                    if keycode == WM_LBUTTONUP || keycode == WM_RBUTTONUP {
                        trayicon.show(app.startup.is_enable)?;
                    }
                }
                return Ok(LRESULT(0));
            }
            WM_USER_SWITCH_APPS => {
                debug!("message WM_USER_SWITCH_APPS");
                let app = get_app(hwnd)?;
                let reverse = lparam.0 == 1;
                app.switch_apps(reverse)?;
                if let Some(state) = &app.switch_apps_state {
                    app.apps_painter.paint(&mut app.overlay, state);
                }
            }
            WM_USER_SWITCH_APPS_DONE => {
                debug!("message WM_USER_SWITCH_APPS_DONE");
                let app = get_app(hwnd)?;
                app.do_switch_app();
            }
            WM_USER_SWITCH_APPS_CANCEL => {
                debug!("message WM_USER_SWITCH_APPS_CANCEL");
                let app = get_app(hwnd)?;
                app.cancel_switch_app();
            }
            WM_USER_SWITCH_WINDOWS => {
                debug!("message WM_USER_SWITCH_WINDOWS");
                let app = get_app(hwnd)?;
                let reverse = lparam.0 == 1;
                let hwnd = app
                    .switch_apps_state
                    .as_ref()
                    .and_then(|state| state.apps.get(state.index).map(|(_, id, _)| *id))
                    .unwrap_or_else(get_foreground_window);
                app.switch_windows(hwnd, reverse)?;
                app.cancel_switch_app();
            }
            WM_USER_SWITCH_WINDOWS_DONE => {
                debug!("message WM_USER_SWITCH_WINDOWS_DONE");
                let app = get_app(hwnd)?;
                app.switch_windows_state.modifier_released = true;
            }
            WM_USER_SWITCH_APP_WINDOWS => {
                debug!("message WM_USER_SWITCH_APP_WINDOWS");
                let app = get_app(hwnd)?;
                let reverse = lparam.0 == 1;
                app.switch_app_windows(reverse)?;
                if let Some(state) = &app.switch_app_windows_state {
                    app.windows_painter.paint(&mut app.overlay, state);
                }
            }
            WM_USER_SWITCH_APP_WINDOWS_DONE => {
                debug!("message WM_USER_SWITCH_APP_WINDOWS_DONE");
                let app = get_app(hwnd)?;
                app.do_switch_app_window();
            }
            WM_USER_SWITCH_APP_WINDOWS_CANCEL => {
                debug!("message WM_USER_SWITCH_APP_WINDOWS_CANCEL");
                let app = get_app(hwnd)?;
                app.cancel_switch_app_window();
            }
            WM_NCHITTEST => {
                return Ok(LRESULT(HTCLIENT as _));
            }
            WM_LBUTTONUP => {
                let app = get_app(hwnd)?;
                app.click();
            }
            WM_COMMAND => {
                let value = wparam.0 as u32;
                let kind = ((value >> 16) & 0xffff) as u16;
                let id = value & 0xffff;
                if kind == 0 {
                    match id {
                        IDM_EXIT => {
                            if let Ok(app) = get_app(hwnd) {
                                unsafe { drop(Box::from_raw(app)) }
                            }
                            unsafe { PostQuitMessage(0) }
                        }
                        IDM_STARTUP => {
                            let app = get_app(hwnd)?;
                            app.startup.toggle()?;
                        }
                        IDM_CONFIGURE => {
                            if let Err(err) = edit_config_file() {
                                alert!("{err}");
                            }
                        }
                        _ => {}
                    }
                }
            }
            WM_ERASEBKGND => {
                return Ok(LRESULT(0));
            }
            _ if msg == WM_USER_REGISTER_TRAYICON || unsafe { msg == WM_TASKBARCREATED } => {
                let app = get_app(hwnd)?;
                app.set_trayicon();
            }
            _ => {}
        }
        Ok(unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
    }

    fn switch_windows(&mut self, hwnd: HWND, reverse: bool) -> Result<bool> {
        let windows = list_windows(
            self.config.switch_windows_ignore_minimal,
            self.config.switch_windows_only_current_desktop(),
            self.is_admin,
        )?;
        debug!(
            "switch windows: hwnd:{hwnd:?} reverse:{reverse} state:{:?}",
            self.switch_windows_state
        );
        let module_path = match windows
            .iter()
            .find(|(_, v)| v.iter().any(|(id, _)| *id == hwnd))
            .map(|(k, _)| k.clone())
        {
            Some(v) => v,
            None => return Ok(false),
        };
        match windows.get(&module_path) {
            None => Ok(false),
            Some(windows) => {
                let windows_len = windows.len();
                if windows_len == 1 {
                    return Ok(false);
                }
                let current_id = windows[0].0;
                let mut index = 1;
                let mut state_id = current_id;
                let mut state_windows = vec![];
                if windows_len > 2 {
                    if let Some((cache_module_path, cache_id, cache_index, cache_windows)) =
                        self.switch_windows_state.cache.as_ref()
                    {
                        if cache_module_path == &module_path {
                            if self.switch_windows_state.modifier_released {
                                if *cache_id != current_id {
                                    if let Some((i, _)) =
                                        windows.iter().enumerate().find(|(_, (v, _))| v == cache_id)
                                    {
                                        index = i;
                                    }
                                }
                            } else {
                                state_id = *cache_id;
                                let mut windows_set: IndexSet<isize> =
                                    windows.iter().map(|(v, _)| v.0 as _).collect();
                                for id in cache_windows {
                                    if windows_set.contains(id) {
                                        state_windows.push(*id);
                                        windows_set.swap_remove(id);
                                    }
                                }
                                state_windows.extend(windows_set);
                                index = if reverse {
                                    if *cache_index == 0 {
                                        windows_len - 1
                                    } else {
                                        cache_index - 1
                                    }
                                } else if *cache_index >= windows_len - 1 {
                                    0
                                } else {
                                    cache_index + 1
                                };
                            }
                        }
                    }
                }
                if state_windows.is_empty() {
                    state_windows = windows.iter().map(|(v, _)| v.0 as _).collect();
                }
                let hwnd = HWND(state_windows[index] as _);
                self.switch_windows_state = SwitchWindowsState {
                    cache: Some((module_path.clone(), state_id, index, state_windows)),
                    modifier_released: false,
                };
                set_foreground_window(hwnd);

                Ok(true)
            }
        }
    }

    fn switch_apps(&mut self, reverse: bool) -> Result<()> {
        debug!(
            "switch apps: reverse:{reverse}, state:{:?}",
            self.switch_apps_state
        );
        if let Some(state) = self.switch_apps_state.as_mut() {
            if reverse {
                if state.index == 0 {
                    state.index = state.apps.len() - 1;
                } else {
                    state.index -= 1;
                }
            } else if state.index == state.apps.len() - 1 {
                state.index = 0;
            } else {
                state.index += 1;
            };
            debug!("switch apps: new index:{}", state.index);
            return Ok(());
        }
        let windows = list_windows(
            self.config.switch_apps_ignore_minimal,
            self.config.switch_apps_only_current_desktop(),
            self.is_admin,
        )?;
        let mut apps = vec![];
        for (module_path, hwnds) in windows.iter() {
            let (module_hwnd, rep_title) = if is_iconic_window(hwnds[0].0) {
                let last = &hwnds[hwnds.len() - 1];
                (last.0, last.1.as_str())
            } else {
                (hwnds[0].0, hwnds[0].1.as_str())
            };
            let name = app_name(module_path, rep_title);
            let module_hicon = self
                .cached_icons
                .entry(module_path.clone())
                .or_insert_with(|| {
                    get_app_icon(
                        &self.config.switch_apps_override_icons,
                        module_path,
                        module_hwnd,
                    )
                });
            apps.push((*module_hicon, module_hwnd, name));
        }
        let num_apps = apps.len() as i32;
        if num_apps == 0 {
            return Ok(());
        }

        let index = if apps.len() == 1 {
            0
        } else if reverse {
            apps.len() - 1
        } else {
            1
        };

        let state = SwitchAppsState { apps, index };
        self.switch_apps_state = Some(state);
        debug!("switch apps, new state:{:?}", self.switch_apps_state);
        Ok(())
    }

    fn click(&mut self) {
        if let Some(state) = self.switch_apps_state.as_mut() {
            if let Some(i) = self.apps_painter.find_clicked_app_index(&self.overlay, state) {
                state.index = i;
                self.do_switch_app();
            }
        }
    }

    fn do_switch_app(&mut self) {
        if let Some(state) = self.switch_apps_state.take() {
            if let Some((_, id, _)) = state.apps.get(state.index) {
                set_foreground_window(*id);
            }
            drop(state);
            self.overlay.hide();
        }
    }

    fn cancel_switch_app(&mut self) {
        if self.switch_apps_state.take().is_some() {
            self.overlay.hide();
        }
    }

    /// Builds (or advances) the vertical popup listing the current app's windows.
    fn switch_app_windows(&mut self, reverse: bool) -> Result<()> {
        if let Some(state) = self.switch_app_windows_state.as_mut() {
            if reverse {
                if state.index == 0 {
                    state.index = state.items.len() - 1;
                } else {
                    state.index -= 1;
                }
            } else if state.index == state.items.len() - 1 {
                state.index = 0;
            } else {
                state.index += 1;
            }
            debug!("switch app windows: new index:{}", state.index);
            return Ok(());
        }

        let foreground = get_foreground_window();
        let windows = list_windows(
            self.config.switch_apps_ignore_minimal,
            self.config.switch_apps_only_current_desktop(),
            self.is_admin,
        )?;
        let (module_path, app_windows) = match windows
            .iter()
            .find(|(_, v)| v.iter().any(|(id, _)| *id == foreground))
        {
            Some((k, v)) => (k.clone(), v),
            None => return Ok(()),
        };

        let module_hicon = *self
            .cached_icons
            .entry(module_path.clone())
            .or_insert_with(|| {
                get_app_icon(
                    &self.config.switch_apps_override_icons,
                    &module_path,
                    foreground,
                )
            });

        let titles = strip_app_name(app_windows.iter().map(|(_, t)| t.as_str()));
        let items: Vec<(HICON, HWND, String)> = app_windows
            .iter()
            .zip(titles)
            .map(|((id, _), title)| (module_hicon, *id, truncate_title(&title)))
            .collect();
        if items.is_empty() {
            return Ok(());
        }

        let index = if items.len() == 1 {
            0
        } else if reverse {
            items.len() - 1
        } else {
            1
        };

        self.switch_app_windows_state = Some(SwitchAppWindowsState {
            items,
            index,
            origin: foreground,
        });
        debug!(
            "switch app windows, new state:{:?}",
            self.switch_app_windows_state
        );
        Ok(())
    }

    fn do_switch_app_window(&mut self) {
        if let Some(state) = self.switch_app_windows_state.take() {
            if let Some((_, id, _)) = state.items.get(state.index) {
                set_foreground_window(*id);
            }
            drop(state);
            self.overlay.hide();
        }
    }

    fn cancel_switch_app_window(&mut self) {
        if let Some(state) = self.switch_app_windows_state.take() {
            // Restore focus to where it was: showing the popup can shift the foreground,
            // so ESC must explicitly return to the originally-focused window.
            set_foreground_window(state.origin);
            self.overlay.hide();
        }
    }
}

/// Strips the trailing " - <app name>" that Windows appends to window titles (e.g.
/// "config.rs - window-switcher - Visual Studio Code" -> "config.rs - window-switcher").
/// With several windows we remove only the trailing segment(s) common to ALL of them —
/// reliably the app name — never stripping a title down to nothing. With a single window
/// we drop just the last " - " segment.
fn strip_app_name<'a>(titles: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    const SEP: &str = " - ";
    let parts: Vec<Vec<&str>> = titles.into_iter().map(|t| t.split(SEP).collect()).collect();
    let min_len = parts.iter().map(|p| p.len()).min().unwrap_or(0);

    // Count trailing segments shared by every title (the app name, possibly multi-word).
    let mut common = 0;
    if parts.len() > 1 {
        while common + 1 < min_len {
            let seg = parts[0][parts[0].len() - 1 - common];
            if parts.iter().all(|p| p[p.len() - 1 - common] == seg) {
                common += 1;
            } else {
                break;
            }
        }
    }
    // Single window: drop just the last segment. Multiple: drop the common suffix.
    let drop = if parts.len() == 1 { 1 } else { common };

    parts
        .iter()
        .map(|p| {
            let keep = p.len().saturating_sub(drop).max(1);
            p[..keep].join(SEP)
        })
        .collect()
}

/// Friendly application name for the app-switcher's selected-item label, taken from the
/// executable's version resource (FileDescription, e.g. "Visual Studio Code", then
/// ProductName). Falls back to the title's trailing " - <app>" segment, then the file stem.
fn app_name(module_path: &str, title: &str) -> String {
    if let Some(name) = exe_version_name(module_path) {
        return name;
    }
    let from_title = title.rsplit(" - ").next().unwrap_or("").trim();
    if !from_title.is_empty() {
        return from_title.to_string();
    }
    let exe = exe_path(module_path);
    let stem = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    stem.strip_suffix(".exe")
        .or_else(|| stem.strip_suffix(".EXE"))
        .unwrap_or(stem)
        .to_string()
}

/// The real executable path from a module key, dropping any browser-profile suffix
/// (e.g. "chrome.exe::Profile2" -> "chrome.exe").
fn exe_path(module_path: &str) -> &str {
    module_path.split("::").next().unwrap_or(module_path)
}

/// Reads the executable's version resource and returns its FileDescription (preferred,
/// the friendly name Explorer shows) or ProductName. `None` if the file has no version info.
fn exe_version_name(module_path: &str) -> Option<String> {
    let path = exe_path(module_path);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let filename = PCWSTR(wide.as_ptr());
    unsafe {
        let size = GetFileVersionInfoSizeW(filename, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(filename, None, size, data.as_mut_ptr() as *mut _).ok()?;
        let block = data.as_ptr() as *const _;

        // Languages to try: the file's own translations first, then English fallbacks.
        let mut langs: Vec<(u16, u16)> = Vec::new();
        let mut trans: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut trans_len: u32 = 0;
        if VerQueryValueW(block, w!("\\VarFileInfo\\Translation"), &mut trans, &mut trans_len)
            .as_bool()
            && !trans.is_null()
        {
            let words = std::slice::from_raw_parts(trans as *const u16, (trans_len / 2) as usize);
            for pair in words.chunks_exact(2) {
                langs.push((pair[0], pair[1]));
            }
        }
        langs.push((0x0409, 0x04b0)); // US English, Unicode
        langs.push((0x0409, 0x04e4)); // US English, Windows multilingual

        for field in ["FileDescription", "ProductName"] {
            for (lang, cp) in &langs {
                let sub = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{field}");
                let sub_w: Vec<u16> = sub.encode_utf16().chain(std::iter::once(0)).collect();
                let mut val: *mut std::ffi::c_void = std::ptr::null_mut();
                let mut val_len: u32 = 0;
                if VerQueryValueW(block, PCWSTR(sub_w.as_ptr()), &mut val, &mut val_len).as_bool()
                    && !val.is_null()
                    && val_len > 0
                {
                    let chars = std::slice::from_raw_parts(val as *const u16, val_len as usize);
                    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
                    let name = String::from_utf16_lossy(&chars[..end]);
                    let name = name.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Caps a window title to a sane length for the popup (counted in chars, not bytes).
fn truncate_title(title: &str) -> String {
    const MAX_TITLE_CHARS: usize = 200;
    if title.chars().count() > MAX_TITLE_CHARS {
        title.chars().take(MAX_TITLE_CHARS).collect()
    } else {
        title.to_string()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        for (_, icon) in self.cached_icons.drain() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

fn get_app(hwnd: HWND) -> Result<&'static mut App> {
    unsafe {
        let ptr = check_error(|| get_window_user_data(hwnd))
            .map_err(|err| anyhow!("Failed to get window ptr, {err}"))?;
        let tx: &mut App = &mut *(ptr as *mut App);
        Ok(tx)
    }
}

#[derive(Debug)]
struct SwitchWindowsState {
    cache: Option<(String, HWND, usize, Vec<isize>)>,
    modifier_released: bool,
}

#[derive(Debug)]
pub struct SwitchAppsState {
    /// Each app: its icon, a representative window handle, and the app's display name.
    pub apps: Vec<(HICON, HWND, String)>,
    pub index: usize,
}

#[derive(Debug)]
pub struct SwitchAppWindowsState {
    /// Each row: the (shared) app icon, the window handle, and its title.
    pub items: Vec<(HICON, HWND, String)>,
    pub index: usize,
    /// The foreground window when the popup opened, restored on cancel.
    pub origin: HWND,
}

#[cfg(test)]
mod tests {
    use super::strip_app_name;

    #[test]
    fn strips_common_app_name_across_windows() {
        let out = strip_app_name([
            "Create plan - guitar-practice-app - Visual Studio Code",
            "deploy.ps1 - configs - Visual Studio Code",
            "config.rs - window-switcher - Visual Studio Code",
        ]);
        assert_eq!(
            out,
            vec![
                "Create plan - guitar-practice-app",
                "deploy.ps1 - configs",
                "config.rs - window-switcher",
            ]
        );
    }

    #[test]
    fn strips_multiword_app_suffix() {
        // The shared suffix may span several " - " segments.
        let out = strip_app_name(["doc1 - Foo - Bar", "doc2 - Foo - Bar"]);
        assert_eq!(out, vec!["doc1", "doc2"]);
    }

    #[test]
    fn single_window_drops_last_segment() {
        let out = strip_app_name(["config.rs - window-switcher - Visual Studio Code"]);
        assert_eq!(out, vec!["config.rs - window-switcher"]);
    }

    #[test]
    fn keeps_titles_without_separator() {
        assert_eq!(strip_app_name(["Claude"]), vec!["Claude"]);
        // Nothing common to strip, and never reduce a title to empty.
        assert_eq!(
            strip_app_name(["Claude", "Settings - Claude"]),
            vec!["Claude", "Settings - Claude"]
        );
    }
}
