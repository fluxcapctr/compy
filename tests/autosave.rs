//! Autosave packages: written from a snapshot on another thread, listed as recoverable, discarded on demand.

use compositor::document::Document;

#[test]
fn autosave_round_trips_lists_and_discards() {
    let home = std::env::temp_dir().join(format!("compositor-test-{}-autosave", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    // This binary alone sets the data home, so the real autosave directory is untouched.
    unsafe { std::env::set_var("XDG_DATA_HOME", &home); }
    let mut d = Document::blank(30, 20, 72.0).unwrap();
    let id = d.add_shape_layer(false, (2.0, 2.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    let mask = compositor::raster::a8_filled(10, 10, 200).unwrap();
    d.renderer.set_mask(id, Some(mask));
    let snapshot = d.autosave_snapshot("Poster").unwrap();
    assert_eq!(snapshot.images.len(), 1);
    assert_eq!(snapshot.masks.len(), 1);
    let path = std::thread::spawn(move || snapshot.write().unwrap()).join().unwrap();
    assert!(path.starts_with(compositor::autosave::dir()) && path.join("manifest.json").exists());
    let listed = compositor::autosave::recoverable();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Poster");
    assert_eq!(listed[0].id, d.document_id);
    assert!(compositor::autosave::is_autosave(&listed[0].path));
    let mut back = Document::new(compositor::format::load(&listed[0].path).unwrap()).unwrap();
    assert_eq!(back.renderer.layers().len(), 2);
    assert!(back.renderer.mask(id).is_some());
    let s = back.renderer.render_flat().unwrap();
    let alpha = compositor::raster::with_bytes(&s, |b, stride| b[5 * stride + 5 * 4 + 3]).unwrap();
    assert!(alpha > 150 && alpha < 220, "masked red square came back: {alpha}");
    back.history.mark_unsaved();
    assert!(back.is_modified(), "a recovered document counts as unsaved");
    compositor::autosave::discard(d.document_id);
    assert!(compositor::autosave::recoverable().is_empty());
    let _ = std::fs::remove_dir_all(&home);
}
