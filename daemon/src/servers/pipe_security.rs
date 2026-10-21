//! Who may open the daemon's named pipe on Windows: the user running the utility and the system,
//! never another user, and never from the network. Without it, the pipe gets the default
//! security, which lets other users of the computer send commands.

use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use log::warn;
use widestring::U16CString;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PWSTR;

/// The pipe's security descriptor, None (the default security, as before) when it can't be
/// built: the utility must keep working.
pub fn pipe_security() -> Option<SecurityDescriptor> {
    let Some(sid) = current_user_sid() else {
        warn!("Unable to find the current user, the IPC pipe keeps the default security");
        return None;
    };
    // Deny the network first, then allow the system and this user (everything else is denied,
    // the P flag stops inherited entries)
    let sddl = format!("D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;{sid})");
    let Ok(wide) = U16CString::from_str(&sddl) else {
        return None;
    };
    match SecurityDescriptor::deserialize(&wide) {
        Ok(descriptor) => Some(descriptor),
        Err(e) => {
            warn!("Unable to build the IPC pipe security ({e}), keeping the default security");
            None
        }
    }
}

/// The SID of the user running this process, as a string (S-1-5-21-..).
fn current_user_sid() -> Option<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let sid = token_user_sid(token);
    let _ = unsafe { CloseHandle(token) };
    sid
}

fn token_user_sid(token: HANDLE) -> Option<String> {
    // First the size, then the TOKEN_USER (in u64s, so it's aligned for the pointers in it)
    let mut size = 0_u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut size) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0_u64; (size as usize).div_ceil(8)];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    }
    .ok()?;
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };

    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.ok()?;
    let sid = unsafe { text.to_string() }.ok();
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    sid
}
