use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eframe::egui;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETFOCUS, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW,
    EndMenu, FindWindowW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowThreadProcessId, HICON, HMENU, IsIconic, LR_DEFAULTCOLOR, MF_STRING, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_RESTORE, SW_SHOW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindowAsync, TPM_BOTTOMALIGN, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, UnregisterClassW, WM_APP,
    WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_PAINT, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

const WINDOW_CLASS: PCWSTR = w!("BlackRune.RenderBridge.Jobs.Tray");
const ICON_ID: u32 = 1;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const OPEN_MESSAGE: u32 = WM_APP + 2;
const IN_MENU_OPEN: usize = 1;
const IN_MENU_EXIT: usize = 2;
const IN_MENU_START_SERVER: usize = 3;
const IN_MENU_STOP_SERVER: usize = 4;
// NIN_KEYSELECT is NIN_SELECT + NINF_KEY in the Windows SDK.
const KEY_SELECT: u32 = NIN_SELECT + 1;

#[derive(Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    StartServer,
    StopServer,
    Exit,
}

pub struct Tray {
    events: Receiver<TrayEvent>,
    window: isize,
    worker: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Tray {
    pub fn start(
        ctx: egui::Context,
        key: &str,
        panel_hwnd: isize,
        server_controls: bool,
    ) -> Result<Self, String> {
        if owned_panel(panel_hwnd).is_none() {
            return Err("The Render Bridge window could not be attached to the tray.".into());
        }
        let (event_sender, events) = mpsc::channel();
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let title = window_title(key);
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&running);
        let worker = thread::Builder::new()
            .name("render-bridge-tray".into())
            .spawn(move || {
                match NativeTray::new(
                    &title,
                    ctx.clone(),
                    event_sender,
                    panel_hwnd,
                    server_controls,
                ) {
                    Ok(tray) if add_initial_icon(tray.window, tray.icon) => {
                        if ready_sender.send(Ok(tray.window.0 as isize)).is_ok() {
                            run_message_loop();
                        }
                        worker_running.store(false, Ordering::Release);
                    }
                    Ok(_tray) => {
                        let _ = ready_sender.send(Err(
                            "Windows could not add the Render Bridge notification-area icon."
                                .into(),
                        ));
                    }
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                    }
                }
                ctx.request_repaint();
            })
            .map_err(|_| "The notification-area thread could not start.".to_owned())?;
        let window = ready
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "The Windows notification area did not respond.".to_owned())??;
        Ok(Self {
            events,
            window,
            worker: Some(worker),
            running,
        })
    }

    pub fn try_recv(&self) -> Option<TrayEvent> {
        self.events.try_recv().ok()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // Only the owning thread removes the icon and destroys its HWND.
        let window = HWND(self.window as *mut _);
        let posted = self.is_running()
            && unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() };
        if let Some(worker) = self.worker.take()
            && (posted || worker.is_finished())
        {
            let _ = worker.join();
        }
    }
}

fn window_title(key: &str) -> HSTRING {
    HSTRING::from(format!("Render Bridge Jobs {key}"))
}

pub fn activate_existing(key: &str, show: bool) -> bool {
    let title = window_title(key);
    // Look up only our hidden callback window. The message can only reopen it.
    unsafe {
        FindWindowW(WINDOW_CLASS, &title).is_ok_and(|window| {
            if !show {
                return true;
            }
            // A shortcut launch may have foreground permission that the existing
            // hidden instance does not have.
            let mut owner = 0;
            if GetWindowThreadProcessId(window, Some(&mut owner)) != 0 {
                let _ = AllowSetForegroundWindow(owner);
            }
            PostMessageW(Some(window), OPEN_MESSAGE, WPARAM(0), LPARAM(0)).is_ok()
        })
    }
}

fn owned_panel(raw: isize) -> Option<HWND> {
    let window = HWND(raw as *mut _);
    let mut owner = 0;
    // Invalid handles produce a zero thread ID. Only act on our own process.
    let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
    (thread != 0 && owner == std::process::id()).then_some(window)
}

struct WindowState {
    context: egui::Context,
    events: Sender<TrayEvent>,
    icon: HICON,
    taskbar_created: u32,
    panel_hwnd: isize,
    server_controls: bool,
}

impl WindowState {
    fn emit(&self, event: TrayEvent) {
        let show_panel = event != TrayEvent::Exit;
        if self.events.send(event).is_ok() {
            if let Some(panel) = owned_panel(self.panel_hwnd) {
                // Restore through the native message queue before asking eframe
                // to paint. Hidden windows may not service repaint requests.
                unsafe {
                    if show_panel {
                        let command = if IsIconic(panel).as_bool() {
                            SW_RESTORE
                        } else {
                            SW_SHOW
                        };
                        let _ = ShowWindowAsync(panel, command);
                        let _ = SetForegroundWindow(panel);
                    } else {
                        // Queue Exit first so the ordinary close handler cannot
                        // interpret this as another request to hide the panel.
                        let _ = PostMessageW(Some(panel), WM_CLOSE, WPARAM(0), LPARAM(0));
                        // Winit handles a queued WM_PAINT as RedrawRequested,
                        // even when Windows would not redraw the hidden window.
                        let _ = PostMessageW(Some(panel), WM_PAINT, WPARAM(0), LPARAM(0));
                    }
                }
            }
            self.context.request_repaint();
        }
    }
}

struct NativeTray {
    window: HWND,
    instance: HINSTANCE,
    icon: HICON,
    state: Option<Box<WindowState>>,
}

impl NativeTray {
    fn new(
        title: &HSTRING,
        context: egui::Context,
        events: Sender<TrayEvent>,
        panel_hwnd: isize,
        server_controls: bool,
    ) -> Result<Self, String> {
        // This thread owns the registered class and every handle created below.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
                .map_err(|_| "The Render Bridge module could not be located.")?
                .into();
            let class = WNDCLASSW {
                hInstance: instance,
                lpszClassName: WINDOW_CLASS,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err("The notification-area window class could not be registered.".into());
            }
            let mut tray = Self {
                window: HWND::default(),
                instance,
                icon: HICON::default(),
                state: None,
            };
            // A hidden top-level window receives Explorer's TaskbarCreated broadcast.
            tray.window = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                WINDOW_CLASS,
                title,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(|_| "The notification-area window could not be created.")?;
            tray.icon =
                create_icon().map_err(|_| "The notification-area icon could not be created.")?;
            tray.state = Some(Box::new(WindowState {
                context,
                events,
                icon: tray.icon,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                panel_hwnd,
                server_controls,
            }));
            if let Some(state) = &tray.state {
                // The owned Box stays alive until Drop clears this window slot.
                SetWindowLongPtrW(
                    tray.window,
                    GWLP_USERDATA,
                    (&**state as *const WindowState) as isize,
                );
            }
            Ok(tray)
        }
    }
}

impl Drop for NativeTray {
    fn drop(&mut self) {
        // These handles were created on this thread, and each is destroyed once.
        unsafe {
            if !self.window.is_invalid() {
                if !self.icon.is_invalid() {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &notification(self.window, self.icon));
                }
                SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.window);
            }
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
            let _ = UnregisterClassW(WINDOW_CLASS, Some(self.instance));
        }
    }
}

fn notification(window: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: icon,
        ..Default::default()
    };
    for (slot, character) in data.szTip.iter_mut().zip("Render Bridge".encode_utf16()) {
        *slot = character;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    data
}

fn add_icon(window: HWND, icon: HICON) -> bool {
    let data = notification(window, icon);
    // The HWND and HICON are live on the tray thread and outlive this call.
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            return false;
        }
        if Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool() {
            return true;
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        false
    }
}

fn add_initial_icon(window: HWND, icon: HICON) -> bool {
    // At sign-in the shortcut can start before Explorer accepts tray icons.
    // Keep the retry within Tray::start's readiness timeout.
    for _ in 0..8 {
        if add_icon(window, icon) {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    add_icon(window, icon)
}

fn run_message_loop() {
    let mut message = MSG::default();
    // GetMessageW returns zero for WM_QUIT and a negative value on error.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        // If the native menu is active, exit its nested loop before teardown.
        let _ = unsafe { EndMenu() };
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // The tray owns this pointer and clears it before destroying the HWND.
    let state =
        unsafe { (GetWindowLongPtrW(window, GWLP_USERDATA) as *const WindowState).as_ref() };
    if let Some(state) = state {
        if message == OPEN_MESSAGE {
            state.emit(TrayEvent::Open);
            return LRESULT(0);
        }
        if message == state.taskbar_created && state.taskbar_created != 0 {
            if !add_icon(window, state.icon) {
                // Keep the panel accessible if Explorer rejects the replacement icon.
                state.emit(TrayEvent::Open);
                unsafe { PostQuitMessage(0) };
            }
            return LRESULT(0);
        }
        if message == TRAY_MESSAGE {
            match icon_notification(lparam) {
                Some(WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | KEY_SELECT) => {
                    state.emit(TrayEvent::Open)
                }
                Some(WM_CONTEXTMENU) => {
                    let position = menu_position(wparam.0);
                    if let Some(event) =
                        show_menu(window, state.icon, position, state.server_controls)
                    {
                        state.emit(event);
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn icon_notification(parameter: LPARAM) -> Option<u32> {
    // NOTIFYICON_VERSION_4 packs the icon ID and event into the high and low words.
    let packed = parameter.0 as u32;
    (packed >> 16 == ICON_ID).then_some(packed & 0xffff)
}

fn menu_position(raw: usize) -> POINT {
    // Version 4 stores signed screen coordinates in the two words of wParam.
    POINT {
        x: (raw as i16).into(),
        y: ((raw >> 16) as i16).into(),
    }
}

fn menu_event(command: usize) -> Option<TrayEvent> {
    match command {
        IN_MENU_OPEN => Some(TrayEvent::Open),
        IN_MENU_START_SERVER => Some(TrayEvent::StartServer),
        IN_MENU_STOP_SERVER => Some(TrayEvent::StopServer),
        IN_MENU_EXIT => Some(TrayEvent::Exit),
        _ => None,
    }
}

fn build_menu(server_controls: bool) -> Option<HMENU> {
    let items = [
        Some((IN_MENU_OPEN, w!("Open Render Bridge"))),
        server_controls.then_some((IN_MENU_START_SERVER, w!("Start server"))),
        server_controls.then_some((IN_MENU_STOP_SERVER, w!("Stop server"))),
        Some((IN_MENU_EXIT, w!("Exit Render Bridge"))),
    ];
    // The tray HWND's thread owns the new menu and its items.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (command, label) in items.into_iter().flatten() {
            if AppendMenuW(menu, MF_STRING, command, label).is_err() {
                let _ = DestroyMenu(menu);
                return None;
            }
        }
        Some(menu)
    }
}

fn show_menu(
    window: HWND,
    icon: HICON,
    mut position: POINT,
    server_controls: bool,
) -> Option<TrayEvent> {
    // Menu creation, display and destruction belong to the tray HWND's thread.
    unsafe {
        if position.x == -1 && position.y == -1 {
            GetCursorPos(&mut position).ok()?;
        }
        let menu = build_menu(server_controls)?;
        let _ = SetForegroundWindow(window);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            position.x,
            position.y,
            None,
            window,
            None,
        )
        .0;
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if command == 0 {
            let _ = Shell_NotifyIconW(NIM_SETFOCUS, &notification(window, icon));
        }
        menu_event(command as usize)
    }
}

const ICON_SIZE: usize = 32;
const HEADER_WORDS: usize = 10;
const ICON_WORDS: usize = HEADER_WORDS + ICON_SIZE * ICON_SIZE + ICON_SIZE;

fn icon_bitmap() -> [u32; ICON_WORDS] {
    // A BITMAPINFOHEADER, little-endian BGRA pixels, then the 1-bit AND mask.
    // Using u32 elements ensures the RT_ICON image is DWORD aligned.
    let mut bitmap = [0; ICON_WORDS];
    bitmap[..HEADER_WORDS].copy_from_slice(&[
        40,
        ICON_SIZE as u32,
        (ICON_SIZE * 2) as u32,
        (32 << 16) | 1,
        0,
        (ICON_SIZE * ICON_SIZE * 4) as u32,
        0,
        0,
        0,
        0,
    ]);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x.abs_diff(x.clamp(7, 24));
            let dy = y.abs_diff(y.clamp(7, 24));
            let pixel = if dx * dx + dy * dy > 36 {
                bitmap[HEADER_WORDS + ICON_SIZE * ICON_SIZE + ICON_SIZE - 1 - y] |= 1 << (x ^ 7);
                0
            } else if (8..=23).contains(&x) && matches!(y, 8..=11 | 14..=17) {
                0xffe0d4f9
            } else if (8..=17).contains(&x) && (20..=23).contains(&y) {
                0xffe0d4f9
            } else if (20..=24).contains(&x) && (20..=24).contains(&y) {
                0xffcbe87a
            } else {
                0xff533585
            };
            bitmap[HEADER_WORDS + (ICON_SIZE - 1 - y) * ICON_SIZE + x] = pixel;
        }
    }
    bitmap
}

fn create_icon() -> windows::core::Result<HICON> {
    let bitmap = icon_bitmap();
    // The DWORD-aligned bitmap remains alive while Windows copies these bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(bitmap.as_ptr().cast(), std::mem::size_of_val(&bitmap))
    };
    unsafe { CreateIconFromResourceEx(bytes, true, 0x00030000, 32, 32, LR_DEFAULTCOLOR) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_icon_bitmap_can_be_loaded_without_a_tray() {
        let bitmap = icon_bitmap();
        assert_eq!(&bitmap[..4], &[40, 32, 64, 0x00200001]);
        assert_eq!(bitmap[HEADER_WORDS], 0);
        assert!(bitmap.contains(&0xff533585));
        let icon = create_icon().unwrap();
        // This test created the icon handle and does not share it with Windows Explorer.
        unsafe { DestroyIcon(icon).unwrap() };
    }

    #[test]
    fn native_menu_uses_signed_positions_and_explicit_commands() {
        let position = menu_position(((900_usize) << 16) | (-150_i16 as u16 as usize));
        assert_eq!((position.x, position.y), (-150, 900));
        assert_eq!(menu_event(IN_MENU_OPEN), Some(TrayEvent::Open));
        assert_eq!(
            menu_event(IN_MENU_START_SERVER),
            Some(TrayEvent::StartServer)
        );
        assert_eq!(menu_event(IN_MENU_STOP_SERVER), Some(TrayEvent::StopServer));
        assert_eq!(menu_event(IN_MENU_EXIT), Some(TrayEvent::Exit));
        assert_eq!(menu_event(0), None);
    }

    #[test]
    fn native_menu_offers_server_actions_only_when_controls_are_available() {
        use windows::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, GetMenuItemID};

        let commands = |server_controls| unsafe {
            let menu = build_menu(server_controls).unwrap();
            let ids: Vec<_> = (0..GetMenuItemCount(Some(menu)))
                .map(|index| GetMenuItemID(menu, index) as usize)
                .collect();
            DestroyMenu(menu).unwrap();
            ids
        };

        assert_eq!(commands(false), [IN_MENU_OPEN, IN_MENU_EXIT]);
        assert_eq!(
            commands(true),
            [
                IN_MENU_OPEN,
                IN_MENU_START_SERVER,
                IN_MENU_STOP_SERVER,
                IN_MENU_EXIT
            ]
        );
    }

    #[test]
    fn native_activation_accepts_shortcut_and_version_four_notifications() {
        use windows::Win32::UI::WindowsAndMessaging::{PM_REMOVE, PeekMessageW, SendMessageW};
        let key = format!("open-fixture-{}", std::process::id());
        let (sender, events) = mpsc::channel();
        // NativeTray::new creates a hidden window but does not register an icon.
        let tray = NativeTray::new(
            &window_title(&key),
            egui::Context::default(),
            sender,
            0,
            false,
        )
        .unwrap();
        assert!(!activate_existing("open-fixture-wrong-target", true));
        let mut message = MSG::default();
        let mut read_open = || {
            // Only pump the private Open message. No GUI input is generated.
            unsafe {
                PeekMessageW(
                    &mut message,
                    Some(tray.window),
                    OPEN_MESSAGE,
                    OPEN_MESSAGE,
                    PM_REMOVE,
                )
            }
            .as_bool()
        };
        assert!(activate_existing(&key, false));
        assert!(!read_open());
        assert!(activate_existing(&key, true));
        assert!(read_open());
        unsafe { DispatchMessageW(&message) };
        assert_eq!(events.try_recv(), Ok(TrayEvent::Open));

        let notify = |id, event| {
            let packed = (id << 16) | event;
            // Dispatch directly to the hidden callback HWND with no desktop input.
            unsafe {
                SendMessageW(
                    tray.window,
                    TRAY_MESSAGE,
                    None,
                    Some(LPARAM(packed as isize)),
                )
            };
        };
        notify(ICON_ID + 1, WM_LBUTTONUP);
        assert!(events.try_recv().is_err());
        for event in [WM_LBUTTONUP, KEY_SELECT] {
            notify(ICON_ID, event);
            assert_eq!(events.try_recv(), Ok(TrayEvent::Open));
        }
    }

    #[test]
    fn tray_exit_queues_close_and_redraw_without_showing_the_window() {
        use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, PM_REMOVE, PeekMessageW};
        let panel = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("Render Bridge hidden panel fixture"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let (sender, events) = mpsc::channel();
        let state = WindowState {
            context: egui::Context::default(),
            events: sender,
            icon: HICON::default(),
            taskbar_created: 0,
            panel_hwnd: panel.0 as isize,
            server_controls: false,
        };
        state.emit(TrayEvent::Exit);
        let mut message = MSG::default();
        let messages = [WM_CLOSE, WM_PAINT].map(|kind| unsafe {
            PeekMessageW(&mut message, Some(panel), kind, kind, PM_REMOVE).as_bool()
        });
        let visible = unsafe { IsWindowVisible(panel).as_bool() };
        unsafe { DestroyWindow(panel).unwrap() };

        assert!(!visible);
        assert_eq!(events.try_recv(), Ok(TrayEvent::Exit));
        assert_eq!(messages, [true, true]);
    }
}
