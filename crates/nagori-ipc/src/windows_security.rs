//! Windows ACL helpers for the IPC pipe and token file.
//!
//! Both the named-pipe server and the token file need to be created with an
//! explicit DACL that restricts access to a known set of SIDs. Without it,
//! Windows applies a default DACL (everyone on the local desktop session
//! can open the pipe; the token file inherits the parent directory's
//! permissions). [`SecurityHandle`] builds a `SECURITY_DESCRIPTOR` with a
//! self-contained DACL and exposes a stable pointer to an embedded
//! `SECURITY_ATTRIBUTES` that can be handed to
//! `ServerOptions::create_with_security_attributes_raw` or `CreateFileW`.
//!
//! The struct owns every byte the descriptor references: the SID buffers,
//! the DWORD-aligned ACL buffer, the `SECURITY_DESCRIPTOR`, and the
//! `SECURITY_ATTRIBUTES`. Moving the struct doesn't invalidate the internal
//! pointers because each part is boxed individually.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, TRUE};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, CreateWellKnownSid, GetLengthSid,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES,
    SECURITY_MAX_SID_SIZE, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Stable Win32 `SECURITY_DESCRIPTOR` revision. `windows-sys` keeps the
/// numeric constant under different module paths across versions; pinning
/// it here avoids the churn (and the value has not changed since NT 4.0).
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

/// `GENERIC_READ` access right (`0x8000_0000`). Pinned here because
/// `windows-sys` 0.61 no longer re-exports it from
/// `Win32::Storage::FileSystem`.
pub const GENERIC_READ: u32 = 0x8000_0000;
/// `GENERIC_WRITE` access right (`0x4000_0000`).
pub const GENERIC_WRITE: u32 = 0x4000_0000;
/// `DELETE` standard access right (`0x0001_0000`). Required on the token
/// file so `MoveFileExW(..., REPLACE_EXISTING)` and `remove_file` can
/// rename or unlink the entry even when the parent directory's ACL does
/// not grant `FILE_DELETE_CHILD` to the daemon.
pub const DELETE: u32 = 0x0001_0000;

/// Opaque-sized `SECURITY_DESCRIPTOR` storage. Windows guarantees the
/// "absolute" form fits in `SECURITY_DESCRIPTOR_MIN_LENGTH` (20) bytes;
/// we round up to 64 to leave slack for future revisions and to keep the
/// buffer comfortably DWORD-aligned via the `u64` element type.
#[repr(C)]
struct SecurityDescriptorStorage {
    storage: [u64; 8],
}

impl SecurityDescriptorStorage {
    const fn new() -> Self {
        Self { storage: [0; 8] }
    }

    const fn as_mut_ptr(&mut self) -> *mut c_void {
        ptr::addr_of_mut!(self.storage).cast()
    }
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// RAII guard for a Windows access token handle.
struct TokenGuard(HANDLE);

impl Drop for TokenGuard {
    fn drop(&mut self) {
        // SAFETY: handle is non-null and was obtained via OpenProcessToken.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn current_user_sid() -> io::Result<Vec<u8>> {
    let mut raw_token: HANDLE = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that does not need
    // releasing; we pass a valid out-pointer for the new token handle.
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            ptr::addr_of_mut!(raw_token),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    let token = TokenGuard(raw_token);

    let mut needed: u32 = 0;
    // SAFETY: passing null + 0-length is the documented way to discover
    // the required buffer size; Windows writes it to `needed` and returns
    // FALSE with ERROR_INSUFFICIENT_BUFFER.
    let _ = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            ptr::null_mut(),
            0,
            ptr::addr_of_mut!(needed),
        )
    };
    if needed == 0 {
        return Err(last_error());
    }

    let mut buf = vec![0_u8; needed as usize];
    // SAFETY: `buf` provides `needed` bytes of writable storage; the token
    // is valid for TOKEN_QUERY.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast::<c_void>(),
            needed,
            ptr::addr_of_mut!(needed),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }

    // The buffer is u8-aligned but TOKEN_USER is 8-aligned; the allocator
    // gives us at least pointer alignment on Vec<u8>, but to satisfy the
    // strict-alignment lint we read the fields through an unaligned copy.
    // SAFETY: GetTokenInformation succeeded with TokenUser, so the first
    // `size_of::<TOKEN_USER>()` bytes of `buf` are a valid TOKEN_USER and
    // its `User.Sid` field points inside `buf` (still valid here).
    let token_user: TOKEN_USER = unsafe { ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>()) };
    let sid_ptr = token_user.User.Sid;
    if sid_ptr.is_null() {
        return Err(io::Error::other("TokenUser returned a null SID"));
    }
    // SAFETY: GetLengthSid accepts any valid PSID and returns its byte
    // length.
    let sid_len = unsafe { GetLengthSid(sid_ptr) };
    if sid_len == 0 {
        return Err(last_error());
    }
    let mut sid = vec![0_u8; sid_len as usize];
    // SAFETY: source has `sid_len` bytes inside `buf`; destination is a
    // freshly allocated buffer of the same length; the two ranges do not
    // overlap.
    unsafe {
        ptr::copy_nonoverlapping(sid_ptr.cast::<u8>(), sid.as_mut_ptr(), sid_len as usize);
    }
    Ok(sid)
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
    let mut size: u32 = SECURITY_MAX_SID_SIZE;
    let mut sid = vec![0_u8; size as usize];
    // SAFETY: `sid` has `size` writable bytes; Windows writes the SID and
    // updates `size` to the actual length.
    let ok = unsafe {
        CreateWellKnownSid(
            kind,
            ptr::null_mut(),
            sid.as_mut_ptr().cast::<c_void>(),
            ptr::addr_of_mut!(size),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    sid.truncate(size as usize);
    Ok(sid)
}

/// Owned `SECURITY_ATTRIBUTES` + backing SIDs / ACL / `SECURITY_DESCRIPTOR`.
///
/// The pointer returned by [`SecurityHandle::as_mut_ptr`] is valid for the
/// lifetime of this value. Drop the handle only after the OS call that
/// consumed the attributes has returned.
pub struct SecurityHandle {
    // SIDs must outlive the ACEs that reference their byte ranges.
    _sids: Vec<Vec<u8>>,
    // ACL buffer; the SECURITY_DESCRIPTOR's Dacl pointer aliases into this.
    _acl: Vec<u32>,
    // Boxed so the SECURITY_ATTRIBUTES `lpSecurityDescriptor` pointer
    // stays valid even when `SecurityHandle` itself moves.
    _descriptor: Box<SecurityDescriptorStorage>,
    attributes: Box<SECURITY_ATTRIBUTES>,
}

impl SecurityHandle {
    /// Build a descriptor whose DACL only grants the current process user
    /// `access_mask` rights.
    pub fn current_user_only(access_mask: u32) -> io::Result<Self> {
        let user = current_user_sid()?;
        Self::from_sids(vec![user], access_mask)
    }

    /// Build a descriptor whose DACL grants the current process user,
    /// `BUILTIN\Administrators`, and `NT AUTHORITY\SYSTEM` the same
    /// `access_mask`. Suitable for the token file: legitimate repair /
    /// MDM tooling can still read it under elevation, but no other
    /// non-elevated user on the desktop can.
    pub fn current_user_admins_system(access_mask: u32) -> io::Result<Self> {
        let user = current_user_sid()?;
        let admins = well_known_sid(WinBuiltinAdministratorsSid)?;
        let system = well_known_sid(WinLocalSystemSid)?;
        Self::from_sids(vec![user, admins, system], access_mask)
    }

    /// Raw pointer to the embedded `SECURITY_ATTRIBUTES`. Stable for the
    /// lifetime of `self`.
    pub fn as_mut_ptr(&mut self) -> *mut SECURITY_ATTRIBUTES {
        ptr::addr_of_mut!(*self.attributes)
    }

    fn from_sids(sids: Vec<Vec<u8>>, access_mask: u32) -> io::Result<Self> {
        if sids.is_empty() {
            return Err(io::Error::other("SecurityHandle requires at least one SID"));
        }

        // ACL layout = sizeof(ACL) header + per-SID:
        //   sizeof(ACCESS_ALLOWED_ACE) covers the ACE header + access mask
        //   + a 4-byte placeholder for the start of the SID. The real SID
        //   replaces that placeholder, so we subtract one DWORD and add
        //   the actual SID byte length.
        let mut acl_bytes: usize = size_of::<ACL>();
        for sid in &sids {
            acl_bytes = acl_bytes
                .checked_add(size_of::<ACCESS_ALLOWED_ACE>())
                .and_then(|n| n.checked_sub(size_of::<u32>()))
                .and_then(|n| n.checked_add(sid.len()))
                .ok_or_else(|| io::Error::other("ACL size overflow"))?;
        }
        let acl_size_u32 =
            u32::try_from(acl_bytes).map_err(|_| io::Error::other("ACL too large"))?;
        // Vec<u32> backing storage gives us DWORD alignment for the ACL.
        let dword_count = acl_bytes.div_ceil(size_of::<u32>());
        let mut acl: Vec<u32> = vec![0; dword_count];
        let acl_ptr: *mut ACL = acl.as_mut_ptr().cast();

        // SAFETY: `acl_ptr` points at `acl_size_u32` writable, DWORD-aligned
        // bytes; InitializeAcl fills in the ACL header.
        let ok = unsafe { InitializeAcl(acl_ptr, acl_size_u32, ACL_REVISION) };
        if ok == 0 {
            return Err(last_error());
        }

        for sid in &sids {
            // SAFETY: sid.as_ptr() points at sid.len() valid SID bytes;
            // AddAccessAllowedAce only reads from the SID buffer despite
            // taking a `*mut` (PSID = *mut c_void), so the const-to-mut
            // cast is sound here.
            let added = unsafe {
                AddAccessAllowedAce(
                    acl_ptr,
                    ACL_REVISION,
                    access_mask,
                    sid.as_ptr().cast::<c_void>().cast_mut(),
                )
            };
            if added == 0 {
                return Err(last_error());
            }
        }

        let mut descriptor = Box::new(SecurityDescriptorStorage::new());
        let descriptor_ptr = descriptor.as_mut_ptr();
        // SAFETY: `descriptor_ptr` references >= SECURITY_DESCRIPTOR_MIN_LENGTH
        // bytes of zeroed storage; the call fills in the header.
        let ok =
            unsafe { InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) };
        if ok == 0 {
            return Err(last_error());
        }
        // SAFETY: descriptor was just initialised; acl_ptr is owned and
        // outlives the descriptor via the SecurityHandle field order.
        let ok = unsafe { SetSecurityDescriptorDacl(descriptor_ptr, TRUE, acl_ptr, FALSE) };
        if ok == 0 {
            return Err(last_error());
        }

        let attributes = Box::new(SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                .expect("SECURITY_ATTRIBUTES fits in u32"),
            lpSecurityDescriptor: descriptor_ptr,
            bInheritHandle: FALSE,
        });

        Ok(Self {
            _sids: sids,
            _acl: acl,
            _descriptor: descriptor,
            attributes,
        })
    }
}
