//! Disk space from the mounted filesystems (`/proc/self/mounts` and
//! `statvfs`), and activity from `/proc/diskstats`.

use std::ffi::CString;

use sidebar_core::Source;
use sidebar_core::source::{DiskSample, Volume};

use crate::traffic::Traffic;

/// Bytes per sector in `/proc/diskstats`, whatever the hardware's size.
const SECTOR: u64 = 512;

pub fn disks() -> Box<dyn Source<DiskSample>> {
    let mut traffic = Traffic::default();
    Box::new(move || {
        let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
        let volumes = mount_points(&mounts).into_iter().filter_map(|(name, path)| volume(name, &path)).collect();
        let stats = std::fs::read_to_string("/proc/diskstats").unwrap_or_default();
        let rates = traffic.update("disks", physical_io(&stats, |name| std::path::Path::new(&format!("/sys/block/{name}/device")).exists()));
        Some(DiskSample { volumes, read_rate: rates.down, write_rate: rates.up })
    })
}

fn volume(name: String, path: &str) -> Option<Volume> {
    let c = CString::new(path).ok()?;
    // SAFETY: a NUL-terminated path and a zeroed out struct.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 || st.f_blocks == 0 {
        return None;
    }
    let block = st.f_frsize as u64;
    Some(Volume { name, used_bytes: (st.f_blocks - st.f_bfree) as u64 * block, total_bytes: st.f_blocks as u64 * block })
}

/// Mount points of real block devices worth showing, one per device (the
/// first mount wins; later ones are bind mounts or btrfs subvolumes), as
/// (display name, path).
fn mount_points(mounts: &str) -> Vec<(String, String)> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(device), Some(path), Some(fs)) = (fields.next(), fields.next(), fields.next()) else { continue };
        let path = unescape(path);
        let boring = !device.starts_with("/dev/")
            || device.starts_with("/dev/loop")
            || matches!(fs, "squashfs" | "iso9660" | "udf")
            || matches!(path.as_str(), "/boot/efi" | "/efi")
            || path.starts_with("/snap/")
            || path.starts_with("/var/snap/");
        if boring || seen.contains(&device) {
            continue;
        }
        seen.push(device);
        out.push((path.clone(), path));
    }
    out
}

/// `/proc/mounts` writes spaces and tabs in paths as octal escapes.
fn unescape(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        match rest.get(i + 1..i + 4).and_then(|o| u8::from_str_radix(o, 8).ok()) {
            Some(b) => {
                out.push(b as char);
                rest = &rest[i + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Bytes (read, written) since boot by the disks `physical` accepts
/// (partitions, device-mapper and loop devices would count twice).
fn physical_io(diskstats: &str, physical: impl Fn(&str) -> bool) -> (u64, u64) {
    let mut total = (0, 0);
    for line in diskstats.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || !physical(f[2]) {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        total.0 += n(5) * SECTOR;
        total.1 += n(9) * SECTOR;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_mount_points() {
        let mounts = "\
sysfs /sys sysfs rw 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/loop3 /snap/core/1 squashfs ro 0 0
/dev/sda1 /mnt/My\\040Files ext4 rw 0 0
/dev/nvme0n1p2 /var/lib/docker ext4 rw 0 0
tmpfs /run tmpfs rw 0 0
";
        let names: Vec<String> = mount_points(mounts).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["/", "/mnt/My Files"]);
    }

    #[test]
    fn sums_physical_disks() {
        let stats = "\
 259       0 nvme0n1 100 0 2000 0 50 0 4000 0 0 0 0 0 0
 259       1 nvme0n1p1 10 0 200 0 5 0 400 0 0 0 0 0 0
   7       0 loop0 1 0 8 0 0 0 0 0 0 0 0 0 0
   8       0 sda 1 0 10 0 1 0 20 0 0 0 0 0 0
";
        let io = physical_io(stats, |n| matches!(n, "nvme0n1" | "sda"));
        assert_eq!(io, (2010 * 512, 4020 * 512));
    }
}
