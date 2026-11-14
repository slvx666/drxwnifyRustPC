#![windows_subsystem = "windows"]
//! Spike: Mini shell on Slint (software renderer, partial redraw). Not product code.
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::rc::Rc;

slint::slint! {
    import { ListView, LineEdit } from "std-widgets.slint";
    export struct Row { title: string, artist: string, cover: image }
    export component Main inherits Window {
        title: "spike-slint";
        preferred-width: 1600px; preferred-height: 900px;
        background: #121216;
        in property <[Row]> rows;
        in property <string> info;
        HorizontalLayout {
            Rectangle { width: 260px; background: #1b1b22;
                VerticalLayout { padding: 12px; spacing: 8px;
                    Text { text: "Библиотека · 書庫"; color: white; font-size: 18px; }
                    LineEdit { placeholder-text: "Поиск / 検索"; }
                    Text { text: info; color: #9a9aa8; wrap: word-wrap; }
                }
            }
            ListView { for r in rows: Rectangle { height: 48px;
                HorizontalLayout { padding: 4px; spacing: 10px;
                    Image { source: r.cover; width: 40px; height: 40px; }
                    VerticalLayout { Text { text: r.title; color: white; } Text { text: r.artist; color: #9a9aa8; } }
                } } }
            Rectangle { width: 320px; background: #1b1b22;
                Text { text: "Текст песни\n夜に駆ける\n밤양갱"; color: white; font-size: 16px; }
            }
        }
    }
}

fn cover(i: u32) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(128, 128);
    for (n, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((n % 128) as u32, (n / 128) as u32);
        *p = Rgba8Pixel { r: (x * 2 + i * 37) as u8, g: (y * 2 + i * 11) as u8, b: (i * 53) as u8, a: 255 };
    }
    Image::from_rgba8(buf)
}

fn main() {
    let info = heavy::touch_all();
    let covers: Vec<Image> = (0..50).map(cover).collect();
    let titles = ["夜に駆ける", "밤양갱", "Кино — Группа крови", "Daft Punk — Get Lucky", "周杰倫 — 晴天"];
    let rows: Vec<Row> = (0..10_000)
        .map(|i| Row {
            title: SharedString::from(format!("{} #{i}", titles[i % titles.len()])),
            artist: "Artist".into(),
            cover: covers[i % 50].clone(),
        })
        .collect();
    let ui = Main::new().unwrap();
    let model = Rc::new(VecModel::from(rows));
    let _ = model.row_count();
    ui.set_rows(ModelRc::from(model));
    ui.set_info(info.into());
    ui.run().unwrap();
}
