//! Dragging files from the Explorer out of the window (onto Windows Explorer, the desktop, another program): Windows' shell
//! makes the drag data for files on disk (`SHCreateDataObject`) and runs the drag with its own default drop source
//! (`SHDoDragDrop`), so no COM object is written here. The drop is always a copy. The call returns when the drop is done.
use std::path::{Path, PathBuf};

/// Drag `files` (all in one folder) out of the window `hwnd`. Returns when the user drops or cancels.
#[cfg(windows)]
pub fn drag_files(hwnd: isize, files: &[PathBuf]) -> Result<(), String> {
    use windows_sys::Win32::System::Ole::{OleInitialize, OleUninitialize};
    let folder = files.first().and_then(|f| f.parent()).ok_or("nothing to drag")?;
    if files.iter().any(|f| f.parent() != Some(folder)) {
        return Err("the dragged files must be in one folder".into());
    }
    // OLE on this thread: the window's thread has it from winit already, then this only counts up and back down
    // SAFETY: plain initialisation call, balanced below when it succeeded
    let ole = unsafe { OleInitialize(std::ptr::null_mut()) } >= 0;
    let result = shell_drag(hwnd, folder, files, true);
    if ole {
        // SAFETY: balances the successful OleInitialize above
        unsafe { OleUninitialize() };
    }
    result
}

#[cfg(windows)]
/// `drag` false: only make (and release) the drag data, for tests: the drag loop waits for a real mouse.
fn shell_drag(hwnd: isize, folder: &Path, files: &[PathBuf], drag: bool) -> Result<(), String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr::null_mut};
    use windows_sys::{
        core::GUID,
        Win32::{
            System::Ole::DROPEFFECT_COPY,
            UI::Shell::{Common::ITEMIDLIST, ILFindLastID, ILFree, SHCreateDataObject, SHDoDragDrop, SHParseDisplayName},
        },
    };
    const IID_IDATAOBJECT: GUID = GUID::from_u128(0x0000010e_0000_0000_c000_000000000046);
    let parse = |p: &Path| -> Option<*mut ITEMIDLIST> {
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
        let mut pidl = null_mut();
        // SAFETY: a NUL-terminated path and an out pointer; the PIDL is freed below with ILFree
        let hr = unsafe { SHParseDisplayName(wide.as_ptr(), null_mut(), &mut pidl, 0, null_mut()) };
        (hr >= 0 && !pidl.is_null()).then_some(pidl)
    };
    let folder_pidl = parse(folder).ok_or_else(|| format!("{}: not found by the shell", folder.display()))?;
    let items: Vec<*mut ITEMIDLIST> = files.iter().filter_map(|f| parse(f)).collect();
    let result = (|| {
        if items.len() != files.len() {
            return Err("a dragged file was not found by the shell".to_string());
        }
        // SAFETY: each item PIDL is absolute; its last part is the child relative to the folder
        let children: Vec<*const ITEMIDLIST> = items.iter().map(|p| unsafe { ILFindLastID(*p) } as *const ITEMIDLIST).collect();
        let mut obj: *mut c_void = null_mut();
        // SAFETY: valid PIDLs, the IDataObject IID and an out pointer
        let hr = unsafe { SHCreateDataObject(folder_pidl, children.len() as u32, children.as_ptr(), null_mut(), &IID_IDATAOBJECT, &mut obj) };
        if hr < 0 || obj.is_null() {
            return Err(format!("the shell could not make the drag data (0x{hr:08x})"));
        }
        if drag {
            let mut effect = 0;
            // SAFETY: our own window, the data object just made; no drop source = the shell's default one
            unsafe { SHDoDragDrop(hwnd as *mut c_void, obj, null_mut(), DROPEFFECT_COPY, &mut effect) };
        }
        // SAFETY: IUnknown::Release is the third entry of every COM object's table
        unsafe {
            let table = *(obj as *const *const usize);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*table.add(2));
            release(obj);
        }
        Ok(())
    })();
    // SAFETY: PIDLs from SHParseDisplayName, each freed once
    unsafe {
        items.iter().for_each(|p| ILFree(*p));
        ILFree(folder_pidl);
    }
    result
}

#[cfg(not(windows))]
pub fn drag_files(_hwnd: isize, _files: &[PathBuf]) -> Result<(), String> {
    Err("dragging out of the window works on Windows only".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn the_shell_makes_drag_data_for_a_real_file_and_refuses_a_missing_one() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_drag_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("adder.ytd");
        std::fs::write(&f, b"x").unwrap();
        // the shell finds the file and makes the drag data (the drag loop itself needs a real mouse: not run here)
        // SAFETY: test thread OLE set-up, balanced
        unsafe { windows_sys::Win32::System::Ole::OleInitialize(std::ptr::null_mut()) };
        assert_eq!(shell_drag(0, &d, std::slice::from_ref(&f), false), Ok(()));
        assert!(shell_drag(0, &d, &[d.join("missing.ytd")], false).is_err());
        unsafe { windows_sys::Win32::System::Ole::OleUninitialize() };
        assert!(drag_files(0, &[]).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
}
