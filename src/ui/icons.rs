//! Tool and panel glyphs drawn as solid shapes in the style of Photoshop CC's tool rail (filled silhouettes at
//! about 16 pixels), in the widget's text color so they follow the theme.

use super::tools::Tool;
use gtk::prelude::*;
use std::f64::consts::{PI, TAU};

pub fn icon(tool: Tool) -> gtk::DrawingArea { glyph_area(17, move |cr| draw(tool, cr)) }

/// A named panel glyph: eye, eye-off, folder, folder-open, triangle-right, triangle-down, new-layer, mask,
/// adjustment, trash, link, fx, new-folder.
pub fn glyph(name: &'static str, size: i32) -> gtk::DrawingArea { glyph_area(size, move |cr| draw_glyph(name, cr)) }

fn glyph_area(size: i32, draw: impl Fn(&cairo::Context) + 'static) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    area.set_draw_func(move |area, cr, w, h| {
        let color = area.color();
        cr.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        // Drawn in a 20 x 20 box, scaled to the icon's size and centered.
        let scale = w.min(h) as f64 / 20.0;
        cr.translate((w as f64 - 20.0 * scale) / 2.0, (h as f64 - 20.0 * scale) / 2.0);
        cr.scale(scale, scale);
        cr.set_line_width(1.5);
        draw(cr);
    });
    area
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// An arrowhead pointing along `angle` with its tip at (x, y).
fn arrow(cr: &cairo::Context, x: f64, y: f64, angle: f64, len: f64) {
    let (c, s) = (angle.cos(), angle.sin());
    cr.move_to(x, y);
    cr.line_to(x - len * c + len * 0.55 * s, y - len * s - len * 0.55 * c);
    cr.line_to(x - len * 0.55 * c, y - len * 0.55 * s);
    cr.line_to(x - len * c - len * 0.55 * s, y - len * s + len * 0.55 * c);
    cr.close_path();
}

fn draw(tool: Tool, cr: &cairo::Context) {
    match tool {
        Tool::Move => {
            // Photoshop's pointer: a filled arrow, with a small move cross beside it.
            cr.move_to(3.0, 2.0); cr.line_to(3.0, 14.0); cr.line_to(6.2, 11.2); cr.line_to(8.6, 16.0); cr.line_to(10.6, 15.0); cr.line_to(8.2, 10.4); cr.line_to(12.4, 10.4); cr.close_path();
            cr.fill().ok();
            let (cx, cy) = (15.0, 15.0);
            cr.set_line_width(1.2);
            cr.move_to(cx - 3.8, cy); cr.line_to(cx + 3.8, cy);
            cr.move_to(cx, cy - 3.8); cr.line_to(cx, cy + 3.8);
            cr.stroke().ok();
            for a in [0.0, PI / 2.0, PI, 3.0 * PI / 2.0] { arrow(cr, cx + 4.6 * a.cos(), cy + 4.6 * a.sin(), a, 2.0); cr.fill().ok(); }
        }
        Tool::Marquee => {
            cr.set_line_width(1.6);
            cr.set_dash(&[2.4, 2.0], 0.5);
            cr.rectangle(3.0, 4.0, 14.0, 12.0);
            cr.stroke().ok();
            cr.set_dash(&[], 0.0);
        }
        Tool::Lasso => {
            // A filled loop with a hole, and the cord trailing off it.
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            cr.save().ok(); cr.translate(10.0, 8.0); cr.scale(7.5, 5.0); cr.arc(0.0, 0.0, 1.0, 0.0, TAU); cr.restore().ok();
            cr.new_sub_path();
            cr.save().ok(); cr.translate(10.0, 8.0); cr.scale(5.5, 3.2); cr.arc(0.0, 0.0, 1.0, 0.0, TAU); cr.restore().ok();
            cr.fill().ok();
            cr.set_line_width(1.8);
            cr.move_to(12.0, 12.6);
            cr.curve_to(13.0, 14.6, 9.6, 15.6, 10.6, 18.4);
            cr.stroke().ok();
        }
        Tool::Wand => {
            // A wand with a star at its tip.
            cr.set_line_width(2.4);
            cr.move_to(3.5, 16.5); cr.line_to(11.0, 9.0);
            cr.stroke().ok();
            star(cr, 14.0, 6.0, 4.2, 1.7);
            cr.fill().ok();
            star(cr, 8.0, 3.5, 1.6, 0.7); cr.fill().ok();
            star(cr, 17.0, 12.0, 1.6, 0.7); cr.fill().ok();
        }
        Tool::Crop => {
            cr.set_line_width(2.4);
            cr.move_to(6.0, 2.0); cr.line_to(6.0, 14.0); cr.line_to(18.0, 14.0);
            cr.move_to(2.0, 6.0); cr.line_to(14.0, 6.0); cr.line_to(14.0, 18.0);
            cr.stroke().ok();
        }
        Tool::Eyedropper => {
            // A pipette: a filled bulb and a tapering tube.
            cr.save().ok(); cr.translate(10.0, 10.0); cr.rotate(PI / 4.0);
            rounded_rect(cr, -2.6, -9.5, 5.2, 6.0, 2.2); cr.fill().ok();
            cr.rectangle(-3.2, -4.0, 6.4, 1.6); cr.fill().ok();
            cr.move_to(-1.8, -2.4); cr.line_to(1.8, -2.4); cr.line_to(0.6, 8.5); cr.line_to(-0.6, 8.5); cr.close_path(); cr.fill().ok();
            cr.restore().ok();
        }
        Tool::Brush => {
            // A brush: the handle, a ferrule band and a full bristle tip.
            cr.save().ok(); cr.translate(10.0, 10.0); cr.rotate(PI / 4.0);
            rounded_rect(cr, -1.3, -10.0, 2.6, 9.0, 1.2); cr.fill().ok();
            cr.rectangle(-2.0, -1.6, 4.0, 2.0); cr.fill().ok();
            cr.move_to(-2.6, 0.8); cr.line_to(2.6, 0.8); cr.curve_to(3.2, 4.0, 1.5, 8.0, 0.0, 9.5); cr.curve_to(-1.5, 8.0, -3.2, 4.0, -2.6, 0.8); cr.close_path(); cr.fill().ok();
            cr.restore().ok();
        }
        Tool::Eraser => {
            // A slanted block with its rubber end.
            cr.save().ok(); cr.translate(10.0, 10.0); cr.rotate(-PI / 4.0);
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            rounded_rect(cr, -8.0, -4.0, 16.0, 8.0, 1.5);
            cr.rectangle(-1.5, -4.0, 6.5, 8.0);
            cr.fill().ok();
            cr.restore().ok();
            cr.set_line_width(1.8);
            cr.move_to(3.0, 17.5); cr.line_to(10.5, 17.5); cr.stroke().ok();
        }
        Tool::Heal => {
            // A bandage: a filled slanted strip with its pad cut out.
            cr.save().ok(); cr.translate(10.0, 10.0); cr.rotate(-PI / 4.0);
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            rounded_rect(cr, -8.5, -3.6, 17.0, 7.2, 3.6);
            cr.rectangle(-3.2, -2.0, 6.4, 4.0);
            cr.fill().ok();
            cr.restore().ok();
        }
        Tool::Clone => {
            // A rubber stamp: the knob, the handle and the base.
            cr.arc(10.0, 5.0, 3.0, 0.0, TAU); cr.fill().ok();
            cr.rectangle(8.6, 6.5, 2.8, 4.5); cr.fill().ok();
            rounded_rect(cr, 3.0, 11.0, 14.0, 4.0, 1.5); cr.fill().ok();
            cr.rectangle(4.5, 15.5, 11.0, 2.0); cr.fill().ok();
        }
        Tool::Blur => {
            // A drop.
            cr.move_to(10.0, 2.5);
            cr.curve_to(10.0, 2.5, 4.0, 10.0, 4.0, 13.0);
            cr.curve_to(4.0, 16.5, 6.7, 18.0, 10.0, 18.0);
            cr.curve_to(13.3, 18.0, 16.0, 16.5, 16.0, 13.0);
            cr.curve_to(16.0, 10.0, 10.0, 2.5, 10.0, 2.5);
            cr.close_path();
            cr.fill().ok();
        }
        Tool::Gradient => {
            // A box running from solid to open.
            // The text color painted through a fading mask, so it follows the theme.
            let g = cairo::LinearGradient::new(3.0, 0.0, 17.0, 0.0);
            g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 1.0);
            g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.08);
            cr.save().ok();
            cr.rectangle(3.0, 4.0, 14.0, 12.0);
            cr.clip();
            cr.mask(&g).ok();
            cr.restore().ok();
            cr.set_line_width(1.2);
            cr.rectangle(3.0, 4.0, 14.0, 12.0);
            cr.stroke().ok();
        }
        Tool::Type => {
            // A serif capital T.
            cr.move_to(3.0, 3.5);
            cr.line_to(17.0, 3.5);
            cr.line_to(17.0, 7.5);
            cr.line_to(15.5, 7.5);
            cr.line_to(15.0, 5.8);
            cr.line_to(11.6, 5.8);
            cr.line_to(11.6, 15.6);
            cr.line_to(13.6, 16.2);
            cr.line_to(13.6, 17.5);
            cr.line_to(6.4, 17.5);
            cr.line_to(6.4, 16.2);
            cr.line_to(8.4, 15.6);
            cr.line_to(8.4, 5.8);
            cr.line_to(5.0, 5.8);
            cr.line_to(4.5, 7.5);
            cr.line_to(3.0, 7.5);
            cr.close_path();
            cr.fill().ok();
        }
        Tool::Shape => {
            // A filled rounded square with a circle cut into its corner.
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            rounded_rect(cr, 3.0, 3.0, 11.0, 11.0, 2.0);
            cr.new_sub_path();
            cr.arc(13.0, 13.0, 4.8, 0.0, TAU);
            cr.fill().ok();
            cr.set_line_width(1.4);
            cr.arc(13.0, 13.0, 4.8, 0.0, TAU);
            cr.stroke().ok();
        }
        Tool::Hand => {
            // A raised hand, filled: palm, four fingers and the thumb.
            cr.move_to(6.0, 17.5);
            cr.line_to(3.5, 12.0);
            cr.curve_to(3.0, 10.5, 5.0, 10.0, 5.5, 11.5);
            cr.line_to(7.0, 13.0);
            cr.line_to(7.0, 4.5);
            cr.curve_to(7.0, 3.0, 9.2, 3.0, 9.2, 4.5);
            cr.line_to(9.2, 9.5);
            cr.line_to(9.2, 3.5);
            cr.curve_to(9.2, 2.0, 11.4, 2.0, 11.4, 3.5);
            cr.line_to(11.4, 9.5);
            cr.line_to(11.4, 4.5);
            cr.curve_to(11.4, 3.0, 13.6, 3.0, 13.6, 4.5);
            cr.line_to(13.6, 10.0);
            cr.line_to(13.6, 6.0);
            cr.curve_to(13.6, 4.5, 15.8, 4.5, 15.8, 6.0);
            cr.line_to(15.8, 13.0);
            cr.curve_to(15.8, 16.0, 13.5, 17.5, 11.0, 17.5);
            cr.close_path();
            cr.fill().ok();
        }
        Tool::Zoom => {
            cr.set_line_width(2.2);
            cr.arc(8.5, 8.5, 5.2, 0.0, TAU);
            cr.stroke().ok();
            cr.set_line_width(3.0);
            cr.move_to(12.6, 12.6); cr.line_to(17.5, 17.5);
            cr.stroke().ok();
            cr.set_line_width(1.6);
            cr.move_to(6.0, 8.5); cr.line_to(11.0, 8.5);
            cr.move_to(8.5, 6.0); cr.line_to(8.5, 11.0);
            cr.stroke().ok();
        }
    }
}

fn star(cr: &cairo::Context, cx: f64, cy: f64, outer: f64, inner: f64) {
    for i in 0..8 {
        let r = if i % 2 == 0 { outer } else { inner };
        let a = i as f64 * PI / 4.0 - PI / 2.0;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 { cr.move_to(x, y); } else { cr.line_to(x, y); }
    }
    cr.close_path();
}

fn draw_glyph(name: &str, cr: &cairo::Context) {
    match name {
        "eye" => {
            // An almond with a pupil.
            cr.move_to(2.0, 10.0);
            cr.curve_to(5.0, 4.5, 15.0, 4.5, 18.0, 10.0);
            cr.curve_to(15.0, 15.5, 5.0, 15.5, 2.0, 10.0);
            cr.close_path();
            cr.set_line_width(1.6);
            cr.stroke().ok();
            cr.arc(10.0, 10.0, 3.0, 0.0, TAU);
            cr.fill().ok();
        }
        "eye-off" => {}
        "triangle-right" => { cr.move_to(7.0, 5.0); cr.line_to(14.0, 10.0); cr.line_to(7.0, 15.0); cr.close_path(); cr.fill().ok(); }
        "triangle-down" => { cr.move_to(5.0, 7.0); cr.line_to(15.0, 7.0); cr.line_to(10.0, 14.0); cr.close_path(); cr.fill().ok(); }
        "folder" => {
            cr.move_to(2.0, 5.0); cr.line_to(8.0, 5.0); cr.line_to(10.0, 7.0); cr.line_to(18.0, 7.0); cr.line_to(18.0, 16.0); cr.line_to(2.0, 16.0); cr.close_path();
            cr.fill().ok();
        }
        "new-layer" => {
            cr.set_line_width(1.5);
            cr.rectangle(3.5, 3.5, 13.0, 13.0); cr.stroke().ok();
            cr.move_to(10.0, 6.5); cr.line_to(10.0, 13.5); cr.move_to(6.5, 10.0); cr.line_to(13.5, 10.0); cr.stroke().ok();
        }
        "new-folder" => {
            cr.set_line_width(1.5);
            cr.move_to(2.5, 5.5); cr.line_to(8.0, 5.5); cr.line_to(10.0, 7.5); cr.line_to(17.5, 7.5); cr.line_to(17.5, 16.0); cr.line_to(2.5, 16.0); cr.close_path();
            cr.stroke().ok();
        }
        "mask" => {
            // A square with a circle cut out: the mask thumbnail.
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            cr.rectangle(3.0, 3.0, 14.0, 14.0);
            cr.new_sub_path();
            cr.arc(10.0, 10.0, 4.5, 0.0, TAU);
            cr.fill().ok();
        }
        "adjustment" => {
            // A half-filled circle.
            cr.set_line_width(1.5);
            cr.arc(10.0, 10.0, 6.5, 0.0, TAU); cr.stroke().ok();
            cr.arc(10.0, 10.0, 6.5, PI / 2.0, 3.0 * PI / 2.0); cr.close_path(); cr.fill().ok();
        }
        "fx" => {
            // The letters fx, as Photoshop marks styled layers.
            cr.select_font_face("Sans", cairo::FontSlant::Italic, cairo::FontWeight::Bold);
            cr.set_font_size(13.0);
            cr.move_to(2.0, 15.0);
            cr.show_text("fx").ok();
        }
        "trash" => {
            cr.rectangle(4.0, 5.0, 12.0, 1.8); cr.fill().ok();
            cr.rectangle(8.0, 3.0, 4.0, 1.8); cr.fill().ok();
            cr.move_to(5.0, 7.5); cr.line_to(15.0, 7.5); cr.line_to(14.0, 17.5); cr.line_to(6.0, 17.5); cr.close_path(); cr.fill().ok();
        }
        "link" => {
            cr.set_line_width(2.0);
            cr.save().ok(); cr.translate(10.0, 10.0); cr.rotate(-PI / 4.0);
            rounded_rect(cr, -7.5, -2.5, 8.0, 5.0, 2.5); cr.stroke().ok();
            rounded_rect(cr, -0.5, -2.5, 8.0, 5.0, 2.5); cr.stroke().ok();
            cr.restore().ok();
        }
        "swap" => {
            cr.set_line_width(1.4);
            cr.arc(10.0, 10.0, 6.0, PI, 3.0 * PI / 2.0); cr.stroke().ok();
            arrow(cr, 10.0, 4.0, 0.0, 3.0); cr.fill().ok();
            cr.arc(10.0, 10.0, 6.0, 0.0, PI / 2.0); cr.stroke().ok();
            arrow(cr, 10.0, 16.0, PI, 3.0); cr.fill().ok();
        }
        "default-colors" => {
            cr.rectangle(3.0, 3.0, 9.0, 9.0); cr.fill().ok();
            cr.set_line_width(1.4);
            cr.rectangle(8.5, 8.5, 8.5, 8.5); cr.stroke().ok();
        }
        _ => {}
    }
}
