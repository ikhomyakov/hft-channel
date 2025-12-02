use std::ptr::NonNull;

pub(crate) fn map_shared_memory(
    shared_memory_name: &str,
    size: usize,
) -> Result<NonNull<u8>, std::io::Error> {
    let addr = unsafe {
        let shared_memory_name = std::ffi::CString::new(shared_memory_name).unwrap();

        let fd = libc::shm_open(
            shared_memory_name.as_ptr(),
            libc::O_CREAT | libc::O_RDWR,
            0o600,
        );
        if fd == -1 {
            return Err(std::io::Error::last_os_error());
        }

        if libc::ftruncate(fd, size as libc::off_t) == -1 {
            let err = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(err);
        }

        let addr = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );

        if addr == libc::MAP_FAILED {
            let err = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(err);
        } else {
            libc::close(fd); // mapping stays valid
        }

        addr
    };

    Ok(NonNull::new(addr as *mut u8).unwrap())
}

pub(crate) unsafe fn unmap_shared_memory(
    ptr: NonNull<u8>,
    size: usize,
) -> Result<(), std::io::Error> {
    unsafe {
        if libc::munmap(ptr.as_ptr().cast(), size) == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
