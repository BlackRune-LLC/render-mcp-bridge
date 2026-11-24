use eframe::egui::{self, ColorImage, Image, Pos2, Rect, TextureHandle, TextureOptions, Ui, Vec2};

const ATLAS: &[u8] = include_bytes!("../assets/render-modes.png");

#[derive(Clone)]
struct Atlas {
    texture: TextureHandle,
    tile_aspect: f32,
}

pub(crate) fn pack_image(ui: &mut Ui, name: &str, size: Vec2) -> bool {
    let (column, row) = match name {
        "Z-Image Turbo" => (0, 0),
        "Wan Animate 2" => (1, 0),
        "BiRefNet" => (0, 1),
        _ => return false,
    };
    tile(ui, column, row, size)
}

pub(crate) fn starter_image(ui: &mut Ui, size: Vec2) -> bool {
    tile(ui, 1, 1, size)
}

fn tile(ui: &mut Ui, column: u8, row: u8, size: Vec2) -> bool {
    let Some(atlas) = atlas(ui.ctx()) else {
        return false;
    };
    let aspect = size.x / size.y;
    let width = (aspect / atlas.tile_aspect).min(1.0) * 0.484;
    let height = (atlas.tile_aspect / aspect).min(1.0) * 0.484;
    let center = Pos2::new(f32::from(column) * 0.5 + 0.25, f32::from(row) * 0.5 + 0.25);
    let crop = Rect::from_center_size(center, Vec2::new(width, height));
    ui.add(
        Image::new((atlas.texture.id(), size))
            .uv(crop)
            .corner_radius(7)
            .alt_text("Illustrative model pack artwork"),
    );
    true
}

fn atlas(ctx: &egui::Context) -> Option<Atlas> {
    let id = egui::Id::new("render-bridge-model-pack-art");
    if let Some(cached) = ctx.data(|data| data.get_temp::<Atlas>(id)) {
        return Some(cached);
    }
    let rgba = image::load_from_memory_with_format(ATLAS, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    let image = ColorImage::from_rgba_unmultiplied([width, height], &rgba);
    let atlas = Atlas {
        texture: ctx.load_texture(
            "render-bridge-model-pack-art",
            image,
            TextureOptions::LINEAR,
        ),
        tile_aspect: width as f32 / height as f32,
    };
    ctx.data_mut(|data| data.insert_temp(id, atlas.clone()));
    Some(atlas)
}
