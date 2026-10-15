//! Cached uid/gid ↔ name lookups.

use std::collections::HashMap;
use std::ffi::{CStr, CString};

use parking_lot::Mutex;

#[derive(Default)]
pub struct NameCache {
    users: Mutex<HashMap<u32, Option<String>>>,
    groups: Mutex<HashMap<u32, Option<String>>>,
}

impl NameCache {
    pub fn user(&self, uid: u32) -> Option<String> {
        self.users
            .lock()
            .entry(uid)
            .or_insert_with(|| user_name(uid))
            .clone()
    }

    pub fn group(&self, gid: u32) -> Option<String> {
        self.groups
            .lock()
            .entry(gid)
            .or_insert_with(|| group_name(gid))
            .clone()
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned())
}

fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(grp.gr_name) }.to_string_lossy().into_owned())
}

/// Resolves a user name or numeric id.
pub fn uid_of(name: &str) -> Option<u32> {
    if let Ok(uid) = name.parse() {
        return Some(uid);
    }
    let cname = CString::new(name).ok()?;
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwnam_r(cname.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result)
    };
    (rc == 0 && !result.is_null()).then_some(pwd.pw_uid)
}

/// Resolves a group name or numeric id.
pub fn gid_of(name: &str) -> Option<u32> {
    if let Ok(gid) = name.parse() {
        return Some(gid);
    }
    let cname = CString::new(name).ok()?;
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getgrnam_r(cname.as_ptr(), &mut grp, buf.as_mut_ptr(), buf.len(), &mut result)
    };
    (rc == 0 && !result.is_null()).then_some(grp.gr_gid)
}
