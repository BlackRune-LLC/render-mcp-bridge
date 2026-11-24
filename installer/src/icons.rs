use eframe::egui::IconData;

const ICON: &[u8] = include_bytes!("../assets/render-bridge.ico");
const WINDOW_SIZE: u8 = 48;

pub fn window_icon() -> IconData {
    let png = icon_frame(WINDOW_SIZE).expect("Render Bridge window icon image");
    eframe::icon_data::from_png_bytes(png).expect("valid Render Bridge window icon")
}

fn icon_frame(size: u8) -> Option<&'static [u8]> {
    // ICO entries use 16-byte table records and byte offsets relative to the file.
    let size = [size, size];
    let count = u16::from_le_bytes(ICON.get(4..6)?.try_into().ok()?) as usize;
    let entry = ICON
        .get(6..6 + count * 16)?
        .chunks_exact(16)
        .find(|entry| entry[..2] == size)?;
    let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
    let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
    ICON.get(offset..offset.checked_add(length)?)
}

#[cfg(windows)]
pub fn tray_icon() -> windows::core::Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};

    const SIZE: u8 = 32;
    let png = icon_frame(SIZE).expect("Render Bridge tray icon image");
    let mut storage = vec![0_u32; png.len().div_ceil(4)];
    // Windows requires DWORD alignment. This initialized allocation is long
    // enough to hold the PNG and lives until Windows copies it into an HICON.
    let bytes = unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast(), png.len()) };
    bytes.copy_from_slice(png);
    unsafe {
        CreateIconFromResourceEx(
            bytes,
            true,
            0x00030000,
            SIZE.into(),
            SIZE.into(),
            LR_DEFAULTCOLOR,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [u8; 6] = [16, 24, 32, 48, 64, 0];

    #[test]
    fn artwork_contains_transparent_frames_at_all_supported_sizes() {
        for size in SIZES {
            let expected = if size == 0 { 256 } else { usize::from(size) };
            let image = eframe::icon_data::from_png_bytes(icon_frame(size).unwrap()).unwrap();
            assert_eq!(
                (image.width as usize, image.height as usize),
                (expected, expected)
            );
            assert_eq!(image.rgba.len(), expected * expected * 4);
            assert!(image.rgba[3] <= 2);
            assert!(
                image
                    .rgba
                    .chunks_exact(4)
                    .any(|pixel| pixel == [167, 245, 228, 255])
            );
        }
        assert_eq!(window_icon().width, u32::from(WINDOW_SIZE));
    }

    #[cfg(all(windows, target_env = "msvc"))]
    #[test]
    fn executable_icon_resources_match_the_shared_artwork() {
        use windows::Win32::System::LibraryLoader::{
            FindResourceW, LoadResource, LockResource, SizeofResource,
        };
        use windows::core::PCWSTR;

        let resource = |kind: usize, id: usize| {
            // Null module refers to the running test executable. Its resource
            // section remains mapped for the duration of the test process.
            unsafe {
                let found =
                    FindResourceW(None, PCWSTR(id as *const u16), PCWSTR(kind as *const u16));
                let length = SizeofResource(None, found) as usize;
                let handle = LoadResource(None, found).unwrap();
                let data = LockResource(handle);
                assert!(!data.is_null() && length != 0);
                std::slice::from_raw_parts(data.cast::<u8>(), length)
            }
        };
        let group = resource(14, 101);
        assert_eq!(group[..6], [0, 0, 1, 0, SIZES.len() as u8, 0]);
        let entries = group[6..].chunks_exact(14);
        assert_eq!(entries.len(), SIZES.len());
        for entry in entries {
            let id = u16::from_le_bytes(entry[12..14].try_into().unwrap());
            assert_eq!(resource(3, id.into()), icon_frame(entry[0]).unwrap());
        }
    }
}
