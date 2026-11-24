//! Windows specifics: console handling (the executable uses the GUI subsystem so the sim can
//! start it without a window), a job object so the browser dies with us, and process lookup.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::IntoRawHandle;
    use std::ptr::null;

    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        AllocConsole, AttachConsole, GetStdHandle, SetConsoleTitleW, SetStdHandle, ATTACH_PARENT_PROCESS,
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn valid(h: HANDLE) -> bool {
        !h.is_null() && h != INVALID_HANDLE_VALUE
    }

    fn open_console(name: &str, access: u32) -> HANDLE {
        let name = wide(name);
        unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        }
    }

    /// Point stdout/stderr (and stdin) at the console we are attached to.
    fn bind_std_handles() {
        let out = open_console("CONOUT$", GENERIC_READ | GENERIC_WRITE);
        if valid(out) {
            unsafe {
                SetStdHandle(STD_OUTPUT_HANDLE, out);
                SetStdHandle(STD_ERROR_HANDLE, out);
            }
        }
        let inp = open_console("CONIN$", GENERIC_READ | GENERIC_WRITE);
        if valid(inp) {
            unsafe { SetStdHandle(STD_INPUT_HANDLE, inp) };
        }
    }

    /// True when stdout already goes somewhere (pipe or file redirection by the parent).
    fn stdout_redirected() -> bool {
        valid(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) })
    }

    pub fn attach_parent_console() -> bool {
        if stdout_redirected() {
            return true;
        }
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0 {
            bind_std_handles();
            return true;
        }
        false
    }

    pub fn setup_console(title: &str) -> bool {
        if attach_parent_console() {
            return false;
        }
        if unsafe { AllocConsole() } != 0 {
            bind_std_handles();
            let t = wide(title);
            unsafe { SetConsoleTitleW(t.as_ptr()) };
            return true;
        }
        false
    }

    pub fn redirect_output(path: &Path) {
        let Ok(file) = std::fs::File::create(path) else { return };
        let h = file.into_raw_handle() as HANDLE;
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, h);
            SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }

    pub fn kill_children_on_exit() {
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            // The handle is intentionally leaked: it closes when this process ends, killing the job.
            if ok == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
                CloseHandle(job);
            }
        }
    }

    fn process_path(pid: u32) -> Option<PathBuf> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            (ok != 0).then(|| PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize])))
        }
    }

    /// Process ids whose executable file name matches `name` (case-insensitive), except our own.
    pub fn find_processes(name: &str) -> Vec<u32> {
        let mut pids = Vec::new();
        let me = unsafe { GetCurrentProcessId() };
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if !valid(snap) {
                return pids;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry);
            while ok != 0 {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if exe.eq_ignore_ascii_case(name) && entry.th32ProcessID != me {
                    pids.push(entry.th32ProcessID);
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
        }
        pids
    }

    pub fn find_processes_by_path(path: &Path) -> Vec<u32> {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return Vec::new() };
        let want = path.to_string_lossy().to_lowercase();
        find_processes(name)
            .into_iter()
            .filter(|&pid| process_path(pid).is_some_and(|p| p.to_string_lossy().to_lowercase() == want))
            .collect()
    }

    pub fn terminate(pid: u32, wait_ms: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let ok = TerminateProcess(h, 1) != 0;
            if ok {
                WaitForSingleObject(h, wait_ms);
            }
            CloseHandle(h);
            ok
        }
    }

    pub fn error_dialog(title: &str, text: &str) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
        };
        let (title, text) = (wide(title), wide(text));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
            );
        }
    }

    pub fn is_elevated() -> bool {
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::OpenProcessToken;
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            );
            CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }

    pub fn run_elevated(exe: &Path, params: &str) -> std::io::Result<u32> {
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE};
        use windows_sys::Win32::UI::Shell::{
            ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
        let verb = wide("runas");
        let file = wide(&exe.to_string_lossy());
        let params = wide(params);
        unsafe {
            CoInitializeEx(null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32);
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE as i32;
            if ShellExecuteExW(&mut info) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if info.hProcess.is_null() {
                return Err(std::io::Error::other("no process was started"));
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            Ok(code)
        }
    }

    pub fn delete_user_registry_key(subkey: &str) -> bool {
        use windows_sys::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
        let subkey = wide(subkey);
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, subkey.as_ptr()) == 0 }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn attach_parent_console() -> bool {
        true
    }
    pub fn setup_console(_title: &str) -> bool {
        false
    }
    pub fn redirect_output(_path: &Path) {}
    pub fn kill_children_on_exit() {}
    pub fn find_processes(_name: &str) -> Vec<u32> {
        Vec::new()
    }
    pub fn find_processes_by_path(_path: &Path) -> Vec<u32> {
        Vec::new()
    }
    pub fn terminate(_pid: u32, _wait_ms: u32) -> bool {
        false
    }
    pub fn error_dialog(_title: &str, _text: &str) {}
    pub fn delete_user_registry_key(_subkey: &str) -> bool {
        false
    }
    pub fn is_elevated() -> bool {
        false
    }
    pub fn run_elevated(_exe: &Path, _params: &str) -> std::io::Result<u32> {
        Err(std::io::Error::other("not supported"))
    }
}

/// Attach to the console of the parent process (when started from a terminal). Returns false
/// when there is none.
pub use imp::attach_parent_console;
/// Use the parent's console, or open a new one. Returns true when a new console was opened
/// (it disappears when we exit, so errors should wait for a key press).
pub use imp::setup_console;
/// Send stdout and stderr to a file (background mode).
pub use imp::redirect_output;
/// Modal error message box, on top of other windows (an installer's).
pub use imp::error_dialog;
/// Delete a key and its subkeys under HKEY_CURRENT_USER. True if it existed and was deleted.
pub use imp::delete_user_registry_key;
/// True when this process runs with administrator rights.
pub use imp::is_elevated;
/// Start `exe` with administrator rights (UAC prompt), wait for it and return its exit code.
/// Cancelling the prompt gives ERROR_CANCELLED (1223).
pub use imp::run_elevated;
/// Put this process (and the browser it starts) in a job that is killed when we exit, however we exit.
pub use imp::kill_children_on_exit;
pub use imp::{find_processes, find_processes_by_path, terminate};
