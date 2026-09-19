//! The layers panel's drags: reorder and nest by drop position, duplicate by drop, copy between projects.

mod common;

use common::*;
use compositor::document::{Document, Place};

fn names(d: &Document) -> Vec<(String, Option<String>)> {
    let layers = d.renderer.layers();
    layers.iter().map(|l| (l.name.clone(), l.parent_id.map(|p| layers.iter().find(|x| x.id == p).unwrap().name.clone()))).collect()
}

fn fixture(name: &str) -> (Document, [uuid::Uuid; 4]) {
    let mut f = Fixture::new(name, 4, 4);
    let a = f.add(Spec { name: "A", size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Default::default() });
    let folder = f.add(Spec { name: "F", group: true, size: (4.0, 4.0), ..Default::default() });
    let b = f.add(Spec { name: "B", size: (4.0, 4.0), pixels: Some(solid(4, 4, BLUE)), parent: Some(folder), ..Default::default() });
    let c = f.add(Spec { name: "C", size: (4.0, 4.0), pixels: Some(solid(4, 4, WHITE)), ..Default::default() });
    (Document::new(f.load().unwrap()).unwrap(), [a, folder, b, c])
}

#[test]
fn drops_reorder_and_nest() {
    let (mut d, [a, folder, b, c]) = fixture("dnd-reorder");
    // A above C at the root: the stack becomes F(B), C, A.
    d.move_layers(&[a], Place::Above(c)).unwrap();
    assert_eq!(names(&d), vec![("F".into(), None), ("B".into(), Some("F".into())), ("C".into(), None), ("A".into(), None)]);
    assert_eq!(d.undo_name(), Some("Reorder Layers"));
    // C into the folder: it joins B, at the top of the folder.
    d.move_layers(&[c], Place::Into(folder)).unwrap();
    assert_eq!(d.renderer.layer(c).parent_id, Some(folder));
    let inside: Vec<_> = d.renderer.layers().iter().filter(|l| l.parent_id == Some(folder)).map(|l| l.name.as_str()).collect();
    assert_eq!(inside, ["B", "C"]);
    // B below A at the root leaves the folder.
    d.move_layers(&[b], Place::Below(a)).unwrap();
    assert_eq!(d.renderer.layer(b).parent_id, None);
    let root: Vec<_> = d.renderer.layers().iter().filter(|l| l.parent_id.is_none()).map(|l| l.name.as_str()).collect();
    assert_eq!(root, ["F", "B", "A"]);
    // A folder cannot go into itself or its own contents, and nothing goes into a pixel layer.
    assert!(d.move_layers(&[folder], Place::Into(folder)).is_err());
    assert!(d.move_layers(&[folder], Place::Into(c)).is_err());
    assert!(d.move_layers(&[a], Place::Into(b)).is_err());
    // Dropping a block onto itself changes nothing.
    d.move_layers(&[a], Place::Above(a)).unwrap();
    d.undo(); d.undo(); d.undo();
    assert_eq!(names(&d)[0].0, "A");
}

#[test]
fn drops_move_a_folder_with_its_contents_and_keep_clipping() {
    let (mut d, [a, folder, b, c]) = fixture("dnd-folder");
    // A second layer inside the folder, clipped to B, so the block carries a clipping link.
    d.select_layer(Some(b));
    let top = d.add_blank_layer();
    assert_eq!(d.renderer.layer(top).parent_id, Some(folder));
    d.toggle_clipping(top);
    assert_eq!(d.renderer.layer(top).mask_source_id, Some(b));
    let _ = c;
    d.move_layers(&[folder], Place::Below(a)).unwrap();
    let order: Vec<_> = d.renderer.layers().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(order[0], "F");
    assert_eq!(d.renderer.layer(b).parent_id, Some(folder));
    assert_eq!(d.renderer.layer(top).mask_source_id, Some(b), "clipping inside the block is untouched");
    // Dragging the folder and its child together keeps the child inside.
    d.move_layers(&[folder, b], Place::Above(a)).unwrap();
    assert_eq!(d.renderer.layer(b).parent_id, Some(folder));
    // Copied to another document, the link is remapped onto the copies.
    let mut other = Document::blank(4, 4, 72.0).unwrap();
    let target = other.active.unwrap();
    other.copy_layers(&d, &[folder], Place::Above(target)).unwrap();
    let new_b = other.renderer.layers().iter().find(|l| l.name == "B").unwrap().id;
    let new_top = other.renderer.layers().iter().find(|l| l.mask_source_id.is_some()).unwrap();
    assert_eq!(new_top.mask_source_id, Some(new_b));
}

#[test]
fn drops_copy_within_and_between_documents() {
    let (mut d, [a, folder, _b, c]) = fixture("dnd-copy");
    let copies = d.copy_layers_within(&[a], Place::Above(c)).unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(d.renderer.layers().len(), 5);
    let copy = d.renderer.layer(copies[0]).clone();
    assert_eq!(copy.name, "A copy");
    assert!(d.has_layer(a), "the original stays");
    assert_eq!(d.active, Some(copies[0]));
    assert_eq!(d.undo_name(), Some("Duplicate Layers"));
    // A folder copied into another document brings its contents, with fresh ids and remapped parents.
    let mut other = Document::blank(4, 4, 72.0).unwrap();
    let target = other.active.unwrap();
    let new_ids = other.copy_layers(&d, &[folder], Place::Above(target)).unwrap();
    assert_eq!(new_ids.len(), 2);
    assert_eq!(other.renderer.layers().len(), 3);
    let new_folder = other.renderer.layers().iter().find(|l| l.name == "F").unwrap();
    let new_b = other.renderer.layers().iter().find(|l| l.name == "B").unwrap();
    assert!(new_folder.is_group() && new_b.parent_id == Some(new_folder.id));
    assert!(!d.renderer.layers().iter().any(|l| l.id == new_folder.id), "fresh ids");
    assert!(other.renderer.has_image(new_b.id));
    assert_eq!(other.undo_name(), Some("Copy Layers"));
    assert!(other.selected.contains(&new_folder.id));
}
