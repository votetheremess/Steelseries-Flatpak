use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const VENDOR_ID: &str = "1038";
const PRODUCT_ID: &str = "2244";
const TARGET_INTERFACE: &str = "input3"; // interface 3

pub struct HidDevice {
    file: File,
    pub path: PathBuf,
}

impl HidDevice {
    pub fn read_timeout(&mut self, buf: &mut [u8; 64], timeout_ms: u64) -> io::Result<usize> {
        use std::os::unix::io::AsRawFd;

        let fd = self.file.as_raw_fd();
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms as i32) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        if ret == 0 {
            return Ok(0); // timeout
        }

        self.file.read(buf)
    }

    pub fn write_command(&mut self, buf: &[u8; 64]) -> io::Result<()> {
        self.file.write_all(buf)
    }

    pub fn try_clone_writer(&self) -> io::Result<HidWriter> {
        let file = fs::OpenOptions::new().write(true).open(&self.path)?;
        Ok(HidWriter { file })
    }
}

pub struct HidWriter {
    file: File,
}

impl HidWriter {
    pub fn write_command(&mut self, buf: &[u8; 64]) -> io::Result<()> {
        self.file.write_all(buf)
    }
}

pub fn find_and_open() -> Result<HidDevice, String> {
    let hidraw_dir = PathBuf::from("/sys/class/hidraw");

    let entries = fs::read_dir(&hidraw_dir)
        .map_err(|e| format!("Cannot read /sys/class/hidraw: {e}"))?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let uevent_path = hidraw_dir
            .join(&name)
            .join("device")
            .join("uevent");

        let Ok(uevent) = fs::read_to_string(&uevent_path) else {
            continue;
        };

        // Check VID/PID match
        let hid_id_match = uevent.lines().any(|line| {
            if let Some(id) = line.strip_prefix("HID_ID=") {
                let id_lower = id.to_lowercase();
                id_lower.contains(&format!("0000{VENDOR_ID}"))
                    && id_lower.contains(&format!("0000{PRODUCT_ID}"))
            } else {
                false
            }
        });

        if !hid_id_match {
            continue;
        }

        // Check interface number via HID_PHYS
        let interface_match = uevent.lines().any(|line| {
            if let Some(phys) = line.strip_prefix("HID_PHYS=") {
                phys.ends_with(TARGET_INTERFACE)
            } else {
                false
            }
        });

        if !interface_match {
            continue;
        }

        let dev_path = PathBuf::from("/dev").join(&name);
        log::info!("Found Arctis Nova Elite at {}", dev_path.display());

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&dev_path)
            .map_err(|e| format!("Failed to open {}: {e}", dev_path.display()))?;

        return Ok(HidDevice {
            file,
            path: dev_path,
        });
    }

    Err("Arctis Nova Elite not found (VID=1038, PID=2244, interface 3)".to_string())
}
