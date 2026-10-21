//! 平台差异封装：目录 fsync 等。

use std::io;
use std::path::Path;

use std::fs::File;

/// 对目录做 fsync，确保新建/删除/改名的目录项落盘（Linux 必需）。
#[cfg(unix)]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// 对目录做 fsync；Windows 无此语义，直接返回成功。
#[cfg(not(unix))]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    let _ = path;
    Ok(())
}

/// 从文件指定偏移读取，填满 `buf`。
#[cfg(unix)]
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

/// 从文件指定偏移读取，填满 `buf`。
#[cfg(windows)]
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
        }
        done += n;
    }
    Ok(())
}
