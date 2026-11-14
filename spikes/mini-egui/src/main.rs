#![windows_subsystem = "windows"]
//! Spike: Mini shell on egui/eframe (glow). Not product code.
use eframe::egui;

struct App { info: String, search: String, covers: Vec<egui::TextureHandle>, titles: Vec<String> }

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        egui::Panel::left("lib").exact_size(260.0).show_inside(ui, |ui| {
            ui.heading("Библиотека · 書庫");
            ui.text_edit_singleline(&mut self.search);
            ui.label(&self.info);
        });
        egui::Panel::right("lyr").exact_size(320.0).show_inside(ui, |ui| {
            ui.label("Текст песни\n夜に駆ける\n밤양갱");
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show_rows(ui, 48.0, 10_000, |ui, range| {
                for i in range {
                    ui.horizontal(|ui| {
                        ui.image((self.covers[i % 50].id(), egui::vec2(40.0, 40.0)));
                        ui.vertical(|ui| { ui.label(&self.titles[i % self.titles.len()]); ui.weak("Artist"); });
                    });
                }
            });
        });
    }
}

fn main() -> eframe::Result {
    let info = heavy::touch_all();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1600.0, 900.0]).with_title("spike-egui"),
        ..Default::default()
    };
    eframe::run_native("spike-egui", opts, Box::new(move |cc| {
        let mut fonts = egui::FontDefinitions::default();
        for (name, path) in [("cjk", r"C:\Windows\Fonts\YuGothM.ttc"), ("kr", r"C:\Windows\Fonts\malgun.ttf")] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
                fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push(name.into());
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let covers = (0..50u32).map(|i| {
            let px: Vec<u8> = (0..128 * 128).flat_map(|n| { let (x, y) = (n % 128, n / 128); [(x * 2 + i * 37) as u8, (y * 2 + i * 11) as u8, (i * 53) as u8, 255] }).collect();
            cc.egui_ctx.load_texture(format!("c{i}"), egui::ColorImage::from_rgba_unmultiplied([128, 128], &px), Default::default())
        }).collect();
        let titles = ["夜に駆ける", "밤양갱", "Кино — Группа крови", "Daft Punk — Get Lucky", "周杰倫 — 晴天"].iter().map(|s| s.to_string()).collect();
        Ok(Box::new(App { info, search: String::new(), covers, titles }))
    }))
}
