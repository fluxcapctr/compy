//! Tool icons drawn as simple line glyphs in the style of the Mac app's SF Symbols (wand.and.stars,
//! rectangle.dashed, lasso, arrow.up.left.and.arrow.down.right, paintbrush.pointed, bandage, seal, drop,
//! hand.draw, magnifyingglass), in the widget's text color so they follow the theme.

use super::tools::Tool;
use gtk::prelude::*;
use std::f64::consts::{PI, TAU};

pub fn icon(tool: Tool) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(20).content_height(20).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    area.set_draw_func(move |area, cr, w, h| {
        let color = area.color();
        cr.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
        cr.set_line_width(1.6);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        // Drawn in a 20 x 20 box, centered.
        cr.translate((w as f64 - 20.0) / 2.0, (h as f64 - 20.0) / 2.0);
        draw(tool, cr);
    });
    area
}

fn sparkle(cr: &cairo::Context, x: f64, y: f64, r: f64) {
    cr.move_to(x - r, y); cr.line_to(x + r, y);
    cr.move_to(x, y - r); cr.line_to(x, y + r);
}

fn draw(tool: Tool, cr: &cairo::Context) {
    match tool {
        Tool::Wand => {
            cr.move_to(3.0, 17.0); cr.line_to(12.0, 8.0);
            cr.stroke().ok();
            cr.set_line_width(2.2);
            cr.move_to(11.0, 9.0); cr.line_to(13.5, 6.5);
            cr.stroke().ok();
            cr.set_line_width(1.4);
            sparkle(cr, 16.0, 4.0, 2.2);
            sparkle(cr, 16.5, 11.5, 1.5);
            sparkle(cr, 9.0, 3.5, 1.4);
            cr.stroke().ok();
        }
        Tool::Marquee => {
            cr.set_dash(&[2.6, 2.2], 1.0);
            cr.rectangle(3.0, 4.0, 14.0, 12.0);
            cr.stroke().ok();
            cr.set_dash(&[], 0.0);
        }
        Tool::Lasso => {
            // The loop, then the rope trailing from its knot.
            cr.save().ok();
            cr.translate(10.0, 8.0);
            cr.scale(7.0, 4.6);
            cr.arc(0.0, 0.0, 1.0, 0.0, TAU);
            cr.restore().ok();
            cr.stroke().ok();
            cr.move_to(11.5, 12.4);
            cr.curve_to(12.5, 14.5, 9.5, 15.5, 10.5, 18.5);
            cr.stroke().ok();
        }
        Tool::Move => {
            cr.move_to(4.0, 4.0); cr.line_to(16.0, 16.0);
            cr.move_to(4.0, 4.0); cr.line_to(9.0, 4.0);
            cr.move_to(4.0, 4.0); cr.line_to(4.0, 9.0);
            cr.move_to(16.0, 16.0); cr.line_to(11.0, 16.0);
            cr.move_to(16.0, 16.0); cr.line_to(16.0, 11.0);
            cr.stroke().ok();
        }
        Tool::Brush => {
            // A pointed brush: the handle, the ferrule, the tip.
            cr.move_to(16.5, 3.5); cr.line_to(8.5, 11.5);
            cr.stroke().ok();
            cr.move_to(8.5, 11.5);
            cr.curve_to(6.0, 10.5, 4.0, 12.5, 4.0, 14.5);
            cr.curve_to(4.0, 16.5, 2.5, 17.0, 2.5, 17.0);
            cr.curve_to(6.0, 17.5, 9.5, 16.0, 9.5, 12.5);
            cr.close_path();
            cr.stroke().ok();
        }
        Tool::Eraser => {
            cr.save().ok();
            cr.translate(10.0, 10.0);
            cr.rotate(-PI / 4.0);
            cr.rectangle(-7.0, -3.5, 14.0, 7.0);
            cr.move_to(-2.0, -3.5); cr.line_to(-2.0, 3.5);
            cr.restore().ok();
            cr.stroke().ok();
            cr.move_to(9.0, 17.5); cr.line_to(17.0, 17.5);
            cr.stroke().ok();
        }
        Tool::Heal => {
            cr.save().ok();
            cr.translate(10.0, 10.0);
            cr.rotate(-PI / 4.0);
            rounded_rect(cr, -8.0, -3.5, 16.0, 7.0, 3.5);
            cr.move_to(-3.0, -3.5); cr.line_to(-3.0, 3.5);
            cr.move_to(3.0, -3.5); cr.line_to(3.0, 3.5);
            cr.restore().ok();
            cr.stroke().ok();
            for (x, y) in [(9.0, 9.0), (11.0, 11.0), (9.0, 11.0), (11.0, 9.0)] { cr.arc(x, y, 0.55, 0.0, TAU); cr.fill().ok(); }
        }
        Tool::Clone => {
            // A seal: a scalloped disc.
            let (cx, cy, r) = (10.0, 10.0, 6.5);
            let points = 12;
            for i in 0..points {
                let a = i as f64 / points as f64 * TAU;
                let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                if i == 0 { cr.move_to(x, y); }
                let b = (i as f64 + 0.5) / points as f64 * TAU;
                cr.curve_to(cx + (r + 1.6) * (a + 0.12).cos(), cy + (r + 1.6) * (a + 0.12).sin(), cx + (r + 1.6) * (b + 0.1).cos(), cy + (r + 1.6) * (b + 0.1).sin(), cx + r * (b + 0.26).cos(), cy + r * (b + 0.26).sin());
            }
            cr.close_path();
            cr.stroke().ok();
            cr.arc(cx, cy, 2.2, 0.0, TAU);
            cr.stroke().ok();
        }
        Tool::Blur => {
            cr.move_to(10.0, 2.5);
            cr.curve_to(10.0, 2.5, 4.0, 10.0, 4.0, 13.0);
            cr.curve_to(4.0, 16.5, 6.7, 18.0, 10.0, 18.0);
            cr.curve_to(13.3, 18.0, 16.0, 16.5, 16.0, 13.0);
            cr.curve_to(16.0, 10.0, 10.0, 2.5, 10.0, 2.5);
            cr.close_path();
            cr.stroke().ok();
        }
        Tool::Hand => {
            // A raised hand: palm, four fingers and the thumb.
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
            cr.stroke().ok();
        }
        Tool::Zoom => {
            cr.arc(8.5, 8.5, 5.5, 0.0, TAU);
            cr.stroke().ok();
            cr.set_line_width(2.2);
            cr.move_to(12.7, 12.7); cr.line_to(17.5, 17.5);
            cr.stroke().ok();
        }
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}
