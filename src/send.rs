//! Typing into the focused app with SendInput, on its own thread.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use qalam::engine::Out;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
    VIRTUAL_KEY, VK_BACK, VK_RETURN, VK_SHIFT, VK_TAB,
};

/// Tag on every event we inject, so our own hook ignores it.
pub const MAGIC: usize = 0x7A_7E_AD;

pub fn spawn() -> Sender<Vec<Out>> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("sender".into()).spawn(move || run(rx)).expect("spawn sender");
    tx
}

fn run(rx: Receiver<Vec<Out>>) {
    for outs in rx {
        let shift_enter = !crate::shared().newline_enter.load(Ordering::Relaxed);
        let inputs = build(&outs, shift_enter);
        // Big batches can be dropped by some apps; send in chunks.
        for chunk in inputs.chunks(200) {
            unsafe { SendInput(chunk, size_of::<INPUT>() as i32) };
        }
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: MAGIC } },
    }
}

fn tap(v: &mut Vec<INPUT>, vk: VIRTUAL_KEY) {
    v.push(key(vk, 0, KEYBD_EVENT_FLAGS(0)));
    v.push(key(vk, 0, KEYEVENTF_KEYUP));
}

fn build(outs: &[Out], shift_enter: bool) -> Vec<INPUT> {
    let mut v = Vec::new();
    for o in outs {
        match o {
            Out::Backspace(n) => (0..*n).for_each(|_| tap(&mut v, VK_BACK)),
            Out::Text(s) => {
                for u in s.encode_utf16() {
                    v.push(key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE));
                    v.push(key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
            Out::Newline if shift_enter => {
                v.push(key(VK_SHIFT, 0, KEYBD_EVENT_FLAGS(0)));
                tap(&mut v, VK_RETURN);
                v.push(key(VK_SHIFT, 0, KEYEVENTF_KEYUP));
            }
            Out::Newline | Out::Enter => tap(&mut v, VK_RETURN),
            Out::Tab => tap(&mut v, VK_TAB),
        }
    }
    v
}
