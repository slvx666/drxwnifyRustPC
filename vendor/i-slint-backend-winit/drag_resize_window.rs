// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

// cSpell: ignore xdir ydir
use i_slint_core::lengths::LogicalLength;
use winit::window::{CursorIcon, ResizeDirection};

pub fn handle_cursor_move_for_resize(
    window: &winit::window::Window,
    position: winit::dpi::PhysicalPosition<f64>,
    current_direction: Option<ResizeDirection>,
    border_width: LogicalLength,
) -> Option<ResizeDirection> {
    if !window.is_decorated() && window.is_resizable() {
        let border_size = f64::from(border_width.get()) * window.scale_factor();
        let location = get_resize_direction(window.inner_size(), position, border_size);

        if current_direction != location {
            window.set_cursor(resize_direction_cursor_icon(location));
        }

        return location;
    }

    None
}

pub fn handle_resize(window: &winit::window::Window, direction: Option<ResizeDirection>) {
    if let Some(dir) = direction {
        // Drxwnify patch: winit's drag_resize_window sets a "dragging" flag that only the end of a size loop
        // clears, so a click on the border released before the loop began disabled resizing (and dragging) for
        // good. The same system call without that flag.
        #[cfg(target_os = "windows")]
        if start_resize_windows(window, dir) {
            return;
        }
        let _ = window.drag_resize_window(dir);
    }
}

#[cfg(target_os = "windows")]
fn start_resize_windows(window: &winit::window::Window, dir: ResizeDirection) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursorPos(p: *mut Point) -> i32;
        fn ReleaseCapture() -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }
    const WM_NCLBUTTONDOWN: u32 = 0x00A1;
    let ht: usize = match dir {
        ResizeDirection::West => 10,
        ResizeDirection::East => 11,
        ResizeDirection::North => 12,
        ResizeDirection::NorthWest => 13,
        ResizeDirection::NorthEast => 14,
        ResizeDirection::South => 15,
        ResizeDirection::SouthWest => 16,
        ResizeDirection::SouthEast => 17,
    };
    let Ok(h) = window.window_handle() else { return false };
    let RawWindowHandle::Win32(h) = h.as_raw() else { return false };
    let mut p = Point { x: 0, y: 0 };
    // SAFETY: plain Win32 calls for a window of this thread; `p` is a valid out pointer.
    unsafe {
        GetCursorPos(&mut p);
        ReleaseCapture();
        let lparam = (((p.y as u16 as u32) << 16) | (p.x as u16 as u32)) as isize;
        PostMessageW(h.hwnd.get(), WM_NCLBUTTONDOWN, ht, lparam);
    }
    true
}

/// Get the cursor icon that corresponds to the resize direction.
fn resize_direction_cursor_icon(resize_direction: Option<ResizeDirection>) -> CursorIcon {
    match resize_direction {
        Some(resize_direction) => match resize_direction {
            ResizeDirection::East => CursorIcon::EResize,
            ResizeDirection::North => CursorIcon::NResize,
            ResizeDirection::NorthEast => CursorIcon::NeResize,
            ResizeDirection::NorthWest => CursorIcon::NwResize,
            ResizeDirection::South => CursorIcon::SResize,
            ResizeDirection::SouthEast => CursorIcon::SeResize,
            ResizeDirection::SouthWest => CursorIcon::SwResize,
            ResizeDirection::West => CursorIcon::WResize,
        },
        None => CursorIcon::Default,
    }
}

fn get_resize_direction(
    win_size: winit::dpi::PhysicalSize<u32>,
    position: winit::dpi::PhysicalPosition<f64>,
    border_size: f64,
) -> Option<ResizeDirection> {
    enum X {
        West,
        East,
        Default,
    }

    enum Y {
        North,
        South,
        Default,
    }

    let xdir = if position.x < border_size {
        X::West
    } else if position.x > (win_size.width as f64 - border_size) {
        X::East
    } else {
        X::Default
    };

    let ydir = if position.y < border_size {
        Y::North
    } else if position.y > (win_size.height as f64 - border_size) {
        Y::South
    } else {
        Y::Default
    };

    Some(match (xdir, ydir) {
        (X::West, Y::North) => ResizeDirection::NorthWest,
        (X::West, Y::South) => ResizeDirection::SouthWest,
        (X::West, Y::Default) => ResizeDirection::West,

        (X::East, Y::North) => ResizeDirection::NorthEast,
        (X::East, Y::South) => ResizeDirection::SouthEast,
        (X::East, Y::Default) => ResizeDirection::East,

        (X::Default, Y::North) => ResizeDirection::North,
        (X::Default, Y::South) => ResizeDirection::South,
        (X::Default, Y::Default) => return None,
    })
}
