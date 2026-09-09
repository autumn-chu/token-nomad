use anyhow::{Context, Result, bail};
use rustix::{
    fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags, fchmod, fstat, mkdirat, openat,
        renameat, renameat_with, statat, unlinkat,
    },
    io::Errno,
};
use std::{
    ffi::{CStr, OsStr, OsString},
    fs,
    io::{Read, Write},
    os::{fd::OwnedFd, unix::ffi::OsStrExt},
    path::{Component, Path, PathBuf},
};

pub struct FileData {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug)]
pub struct DirectoryEntry {
    pub name: OsString,
    pub kind: EntryKind,
}

pub fn read_directory(path: &Path) -> Result<Vec<DirectoryEntry>> {
    let directory = open_directory(path, false)?;
    let mut entries = Vec::new();
    let iterator = Dir::read_from(&directory)?;
    for entry in iterator {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let stat = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let file_type = FileType::from_raw_mode(stat.st_mode);
        let kind = if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Other
        };
        entries.push(DirectoryEntry {
            name: OsString::from(OsStr::from_bytes(name.to_bytes())),
            kind,
        });
    }
    Ok(entries)
}

pub fn entry_kind(path: &Path) -> Result<Option<EntryKind>> {
    let Some((parent, name)) = open_parent(path, false)? else {
        return Ok(None);
    };
    let stat = match statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error).context("Failed to inspect storage entry"),
    };
    let file_type = FileType::from_raw_mode(stat.st_mode);
    Ok(Some(if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }))
}

pub fn entry_mode(path: &Path, expected: EntryKind) -> Result<u32> {
    let entry = open_expected_entry(path, expected)?;
    Ok(u32::from(fstat(entry)?.st_mode & 0o777))
}

pub fn set_entry_mode(path: &Path, mode: u32, expected: EntryKind) -> Result<()> {
    if mode > 0o777 {
        bail!("Invalid file mode");
    }
    let entry = open_expected_entry(path, expected)?;
    fchmod(entry, Mode::from_raw_mode(mode as _))?;
    Ok(())
}

pub fn read_regular(path: &Path, max_bytes: u64) -> Result<Option<FileData>> {
    let Some((parent, name)) = open_parent(path, false)? else {
        return Ok(None);
    };
    let file = match openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to open regular file at {}", path.display()));
        }
    };
    let stat = fstat(&file)?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_size < 0 {
        bail!("Path is not a regular file at {}", path.display());
    }
    if stat.st_size as u64 > max_bytes {
        bail!("File exceeds the size limit at {}", path.display());
    }
    let mut bytes = Vec::with_capacity(stat.st_size as usize);
    fs::File::from(file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("File exceeds the size limit at {}", path.display());
    }
    Ok(Some(FileData {
        bytes,
        mode: u32::from(stat.st_mode & 0o777),
    }))
}

pub fn atomic_write(path: &Path, bytes: &[u8], default_mode: u32) -> Result<()> {
    let (parent, name) = open_parent(path, true)?.context("Atomic write target has no parent")?;
    let mode = existing_regular_mode(&parent, &name, path)?.unwrap_or(default_mode);
    atomic_write_at(&parent, &name, path, bytes, mode)
}

pub fn atomic_write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let (parent, name) = open_parent(path, true)?.context("Atomic write target has no parent")?;
    existing_regular_mode(&parent, &name, path)?;
    atomic_write_at(&parent, &name, path, bytes, mode)
}

pub fn create_new_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let (parent, name) = open_parent(path, true)?.context("New file target has no parent")?;
    if existing_regular_mode(&parent, &name, path)?.is_some() {
        bail!("New file target already exists");
    }
    write_temp_then(&parent, path, bytes, mode, |temp_name| {
        renameat_with(&parent, temp_name, &parent, &name, RenameFlags::NOREPLACE)?;
        Ok(())
    })
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    let directory = open_directory(path, true)?;
    fchmod(&directory, Mode::from_raw_mode(0o700))?;
    Ok(())
}

pub fn create_private_directory(path: &Path) -> Result<()> {
    let (parent, name) = open_parent(path, true)?.context("New directory has no parent")?;
    mkdirat(&parent, &name, Mode::from_raw_mode(0o700))
        .context("Failed to create private directory")?;
    Ok(())
}

pub fn create_lock(path: &Path) -> Result<fs::File> {
    let (parent, name) = open_parent(path, true)?.context("Lock path has no parent")?;
    let file = openat(
        &parent,
        &name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )?;
    fchmod(&file, Mode::from_raw_mode(0o600))?;
    Ok(fs::File::from(file))
}

pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    let Some((parent, name)) = open_parent(path, false)? else {
        return Ok(());
    };
    match unlinkat(&parent, &name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to remove file at {}", path.display()))
        }
    }
}

pub fn remove_regular(path: &Path) -> Result<()> {
    let (parent, name) = open_parent(path, false)?.context("Managed file parent is missing")?;
    let file = openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .with_context(|| format!("Failed to open managed file at {}", path.display()))?;
    if !FileType::from_raw_mode(fstat(&file)?.st_mode).is_file() {
        bail!("Managed path is not a regular file at {}", path.display());
    }
    unlinkat(&parent, &name, AtFlags::empty())
        .with_context(|| format!("Failed to remove managed file at {}", path.display()))?;
    Ok(())
}

pub fn rename_new(source: &Path, destination: &Path) -> Result<()> {
    let (source_parent, source_name) =
        open_parent(source, false)?.context("Source parent is missing")?;
    let (destination_parent, destination_name) =
        open_parent(destination, true)?.context("Destination has no parent")?;
    renameat_with(
        &source_parent,
        &source_name,
        &destination_parent,
        &destination_name,
        RenameFlags::NOREPLACE,
    )
    .with_context(|| {
        format!(
            "Failed to install {} at new destination {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

pub fn exchange_entries(first: &Path, second: &Path) -> Result<()> {
    let (first_parent, first_name) =
        open_parent(first, false)?.context("First exchange parent is missing")?;
    let (second_parent, second_name) =
        open_parent(second, false)?.context("Second exchange parent is missing")?;
    renameat_with(
        &first_parent,
        &first_name,
        &second_parent,
        &second_name,
        RenameFlags::EXCHANGE,
    )
    .with_context(|| {
        format!(
            "Failed to exchange {} with {}",
            first.display(),
            second.display()
        )
    })?;
    Ok(())
}

pub fn remove_tree(path: &Path) -> Result<()> {
    let Some((parent, name)) = open_parent(path, false)? else {
        return Ok(());
    };
    match statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => remove_entry_at(&parent, os_str_to_cstr(&name)?.as_c_str()),
        Err(Errno::NOENT) => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to inspect tree at {}", path.display()))
        }
    }
}

fn atomic_write_at(
    parent: &OwnedFd,
    name: &OsStr,
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    write_temp_then(parent, path, bytes, mode, |temp_name| {
        existing_regular_mode(parent, name, path)?;
        renameat(parent, temp_name, parent, name)?;
        Ok(())
    })
}

fn write_temp_then(
    parent: &OwnedFd,
    path: &Path,
    bytes: &[u8],
    mode: u32,
    install: impl FnOnce(&OsStr) -> Result<()>,
) -> Result<()> {
    if mode > 0o777 {
        bail!("Invalid file mode");
    }
    let temp_name = OsString::from(format!(
        ".nomad-{}-{}.tmp",
        std::process::id(),
        unique_suffix()?
    ));
    let temp = openat(
        parent,
        &temp_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(mode as _),
    )?;
    let result: Result<()> = (|| {
        let mut file = fs::File::from(temp);
        file.write_all(bytes)?;
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        file.sync_all()?;
        install(&temp_name)?;
        rustix::fs::fsync(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let cleanup = unlinkat(parent, &temp_name, AtFlags::empty());
        if let Err(cleanup) = cleanup
            && cleanup != Errno::NOENT
        {
            return result.context(format!("temporary-file cleanup also failed: {cleanup}"));
        }
    }
    result.with_context(|| format!("Failed to replace {} atomically", path.display()))
}

fn existing_regular_mode(parent: &OwnedFd, name: &OsStr, path: &Path) -> Result<Option<u32>> {
    match openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(file) => {
            let stat = fstat(file)?;
            if !FileType::from_raw_mode(stat.st_mode).is_file() {
                bail!(
                    "Atomic write target must be a regular file at {}",
                    path.display()
                );
            }
            Ok(Some(u32::from(stat.st_mode & 0o777)))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to inspect atomic write target at {}",
                path.display()
            )
        }),
    }
}

fn open_expected_entry(path: &Path, expected: EntryKind) -> Result<OwnedFd> {
    let (parent, name) = open_parent(path, false)?.context("Entry parent is missing")?;
    let flags = match expected {
        EntryKind::File => OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        EntryKind::Directory => {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
        }
        EntryKind::Symlink | EntryKind::Other => bail!("Unsupported entry kind for mode access"),
    };
    let entry = openat(&parent, &name, flags, Mode::empty())?;
    let file_type = FileType::from_raw_mode(fstat(&entry)?.st_mode);
    let matches = match expected {
        EntryKind::File => file_type.is_file(),
        EntryKind::Directory => file_type.is_dir(),
        EntryKind::Symlink | EntryKind::Other => false,
    };
    if !matches {
        bail!("Storage entry kind changed");
    }
    Ok(entry)
}

fn open_directory(path: &Path, create: bool) -> Result<OwnedFd> {
    let components = absolute_components(path)?;
    let mut directory = openat(
        CWD,
        Path::new("/"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    for name in components {
        directory = match openat(
            &directory,
            &name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(next) => next,
            Err(Errno::NOENT) if create => {
                match mkdirat(&directory, &name, Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("Failed to create directory at {}", path.display())
                        });
                    }
                }
                openat(
                    &directory,
                    &name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )?
            }
            Err(error) => return Err(error).with_context(|| {
                format!(
                    "Directory path contains a missing, non-directory, or symlink component at {}",
                    path.display()
                )
            }),
        };
    }
    Ok(directory)
}

fn open_parent(path: &Path, create: bool) -> Result<Option<(OwnedFd, OsString)>> {
    let mut components = absolute_components(path)?;
    let Some(name) = components.pop() else {
        bail!("Path must name an entry below the filesystem root");
    };
    let parent_path = components
        .iter()
        .fold(PathBuf::from("/"), |path, component| path.join(component));
    match open_directory(&parent_path, create) {
        Ok(parent) => Ok(Some((parent, name))),
        Err(error) if !create && error.downcast_ref::<Errno>() == Some(&Errno::NOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

fn absolute_components(path: &Path) -> Result<Vec<OsString>> {
    if !path.is_absolute() {
        bail!("Storage paths must be absolute");
    }
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        bail!("Storage path is invalid");
    }
    components
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            _ => bail!("Storage paths may not traverse directories"),
        })
        .collect()
}

fn remove_entry_at(parent: &OwnedFd, name: &CStr) -> Result<()> {
    let stat = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode).is_dir() {
        let child = openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        let entries = Dir::read_from(&child)?;
        for entry in entries {
            let entry = entry?;
            if entry.file_name().to_bytes() != b"." && entry.file_name().to_bytes() != b".." {
                remove_entry_at(&child, entry.file_name())?;
            }
        }
        unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    } else {
        unlinkat(parent, name, AtFlags::empty())?;
    }
    Ok(())
}

fn os_str_to_cstr(value: &OsStr) -> Result<std::ffi::CString> {
    std::ffi::CString::new(value.as_bytes()).context("Storage path contains a null byte")
}

fn unique_suffix() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ))
}
