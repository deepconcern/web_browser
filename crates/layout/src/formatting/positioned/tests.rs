//! Absolute, fixed, relative and sticky positioning tests.

use crate::formatting::float::tests::{RESET, assert_rect, lay, rect};
use crate::*;

#[test]
fn absolute_insets_against_positioned_ancestor_padding_box() {
    let l = lay(
        r#"<div id=cb><p>flow</p><div id=a></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; margin: 10px; width: 300px; height: 200px; \
             border: 5px solid; padding: 20px }} \
             #a {{ position: absolute; left: 10px; top: 15px; width: 50px; height: 40px }}"
        ),
    );
    // The padding box starts at 10 + 5 = 15.
    assert_rect(l.rect("a"), rect(25.0, 30.0, 50.0, 40.0));
    // Out of flow: the container's height is unaffected and the fragment
    // is a child of the containing block's fragment.
    assert_eq!(l.rect("cb").height, 250.0);
    assert_eq!(l.frag("a").parent, Some(l.id_of("cb")));
}

#[test]
fn right_and_bottom_insets() {
    let l = lay(
        r#"<div id=cb><div id=a></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; width: 300px; height: 200px }} \
             #a {{ position: absolute; right: 10px; bottom: 20px; width: 50px; height: 40px; margin: 5px }}"
        ),
    );
    assert_rect(
        l.rect("a"),
        rect(
            300.0 - 10.0 - 5.0 - 50.0,
            200.0 - 20.0 - 5.0 - 40.0,
            50.0,
            40.0,
        ),
    );
}

#[test]
fn auto_width_shrinks_to_fit_or_stretches_between_insets() {
    let l = lay(
        r#"<div id=cb><div id=s>abc def</div><div id=t>abc</div><div id=c></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; width: 400px; height: 300px }} \
             #s {{ position: absolute; left: 0; top: 0 }} \
             #t {{ position: absolute; left: 10px; right: 30px; top: 50px; bottom: 50px }} \
             #c {{ position: absolute; left: 0; right: 0; width: 100px; height: 10px; margin: 0 auto; top: 0 }}"
        ),
    );
    // Shrink-to-fit: max-content of "abc def" is 56px.
    assert_rect(l.rect("s"), rect(0.0, 0.0, 56.0, 16.0));
    // Both insets set: the width and height fill the rest.
    assert_rect(l.rect("t"), rect(10.0, 50.0, 360.0, 200.0));
    // Auto margins centre a sized box between insets.
    assert_rect(l.rect("c"), rect(150.0, 0.0, 100.0, 10.0));
}

#[test]
fn static_position_when_insets_are_auto() {
    let l = lay(
        r#"<div id=cb><p id=p>line</p><div id=wrap><div id=a>abs</div><p id=q>q</p></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; padding: 10px }} #wrap {{ margin-left: 30px }} \
             #a {{ position: absolute }}"
        ),
    );
    // Where it would have been in flow: after #p, in #wrap's content box.
    assert_eq!(l.rect("a").x, 40.0);
    assert_eq!(l.rect("a").y, 26.0);
    // It takes no room: #q starts there too.
    assert_eq!(l.rect("q").y, 26.0);
}

#[test]
fn no_positioned_ancestor_uses_initial_containing_block() {
    let l = lay(
        r#"<div id=a></div>"#,
        "body { margin: 20px } #a { position: absolute; right: 0; bottom: 0; width: 10px; height: 10px }",
    );
    assert_rect(l.rect("a"), rect(790.0, 590.0, 10.0, 10.0));
}

#[test]
fn fixed_is_relative_to_viewport_not_ancestors() {
    let l = lay(
        r#"<div id=cb><div id=f></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; margin: 50px; height: 1000px }} \
             #f {{ position: fixed; top: 5px; left: 5px; width: 100%; height: 10% }}"
        ),
    );
    assert_rect(l.rect("f"), rect(5.0, 5.0, 800.0, 60.0));
    let root = l.tree.root().expect("root");
    assert_eq!(l.frag("f").parent, Some(root));
    assert!(l.frag("f").stacking.creates_stacking_context);
}

#[test]
fn absolute_escapes_overflow_clip_of_non_positioned_ancestor() {
    let l = lay(
        r#"<div id=cb><div id=clip><div id=a></div></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; width: 200px; height: 200px }} \
             #clip {{ overflow: hidden; width: 50px; height: 50px; margin: 10px }} \
             #a {{ position: absolute; top: 100px; left: 100px; width: 20px; height: 20px }}"
        ),
    );
    assert_eq!(l.frag("a").parent, Some(l.id_of("cb")));
    // #clip's top margin collapses through #cb, which moves down 10px.
    assert_eq!(l.rect("cb").y, 10.0);
    assert_rect(l.rect("a"), rect(100.0, 110.0, 20.0, 20.0));
}

#[test]
fn nested_absolute_uses_nearest_positioned_ancestor() {
    let l = lay(
        r#"<div id=outer><div id=inner><div id=a></div></div></div>"#,
        &format!(
            "{RESET} #outer {{ position: relative; padding: 30px }} \
             #inner {{ position: absolute; left: 100px; top: 100px; width: 50px; height: 50px }} \
             #a {{ position: absolute; left: 5px; top: 5px; width: 1px; height: 1px }}"
        ),
    );
    assert_rect(l.rect("inner"), rect(100.0, 100.0, 50.0, 50.0));
    assert_rect(l.rect("a"), rect(105.0, 105.0, 1.0, 1.0));
}

#[test]
fn absolute_auto_height_from_content_and_percentages() {
    let l = lay(
        r#"<div id=cb><div id=a><div id=half></div><p>x</p></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; width: 400px; height: 300px }} \
             #a {{ position: absolute; top: 10px; left: 10px; width: 50% }} \
             #half {{ height: 20px }}"
        ),
    );
    assert_rect(l.rect("a"), rect(10.0, 10.0, 200.0, 36.0));
    let l = lay(
        r#"<div id=cb><div id=a><div id=half></div></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; height: 300px }} \
             #a {{ position: absolute; top: 0; bottom: 100px; width: 10px }} \
             #half {{ height: 50% }}"
        ),
    );
    // top + bottom make the height definite (200px) for percentages.
    assert_eq!(l.rect("half").height, 100.0);
}

#[test]
fn min_and_max_constraints_apply() {
    let l = lay(
        r#"<div id=cb><div id=a></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; width: 400px; height: 300px }} \
             #a {{ position: absolute; left: 0; right: 0; top: 0; bottom: 0; max-width: 100px; min-height: 350px }}"
        ),
    );
    assert_rect(l.rect("a"), rect(0.0, 0.0, 100.0, 350.0));
}

#[test]
fn absolute_replaced_element_keeps_its_size() {
    let l = lay(
        r#"<div id=cb><img id=i width=40 height=30></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative; width: 400px; height: 300px }} \
             #i {{ position: absolute; right: 0; bottom: 0 }}"
        ),
    );
    assert_rect(l.rect("i"), rect(360.0, 270.0, 40.0, 30.0));
}

#[test]
fn stacking_data_for_paint() {
    let l = lay(
        r#"<div id=z></div><div id=auto></div><div id=st></div><div id=op></div><div id=sticky></div>"#,
        &format!(
            "{RESET} #z {{ position: relative; z-index: 2 }} #auto {{ position: absolute }} \
             #st {{ z-index: 5 }} #op {{ opacity: 0.5 }} #sticky {{ position: sticky; top: 7px }}"
        ),
    );
    let z = l.frag("z").stacking;
    assert_eq!(
        (z.positioned, z.z_index, z.creates_stacking_context),
        (true, Some(2), true)
    );
    let a = l.frag("auto").stacking;
    assert_eq!(
        (a.positioned, a.z_index, a.creates_stacking_context),
        (true, None, false)
    );
    // z-index does not apply to a static block.
    let s = l.frag("st").stacking;
    assert_eq!(
        (s.positioned, s.z_index, s.creates_stacking_context),
        (false, None, false)
    );
    let o = l.frag("op").stacking;
    assert!(o.creates_stacking_context && o.opacity == 0.5);
    // Sticky falls back to relative positioning (offset applied) in M1.
    let st = l.frag("sticky");
    assert!(st.stacking.positioned && st.stacking.creates_stacking_context);
    assert_eq!(st.relative_offset, Point::new(0.0, 7.0));
}

#[test]
fn absolute_does_not_count_in_intrinsic_width() {
    let l = lay(
        r#"<div id=ib><span>ab</span><div id=a>very long absolute text</div></div>"#,
        &format!(
            "{RESET} #ib {{ display: inline-block; position: relative }} #a {{ position: absolute }}"
        ),
    );
    assert_eq!(l.rect("ib").width, 16.0);
}

#[test]
fn fixed_inside_absolute_inside_root() {
    let l = lay(
        r#"<div id=a><div id=f></div></div>"#,
        &format!(
            "{RESET} #a {{ position: absolute; left: 100px; top: 100px }} \
             #f {{ position: fixed; right: 0; top: 0; width: 10px; height: 10px }}"
        ),
    );
    assert_rect(l.rect("f"), rect(790.0, 0.0, 10.0, 10.0));
    // Static position of a fixed box inside an absolute one.
    let l = lay(
        r#"<div id=a><p>x</p><div id=f></div></div>"#,
        &format!(
            "{RESET} #a {{ position: absolute; left: 100px; top: 100px; width: 50px }} \
             #f {{ position: fixed; width: 10px; height: 10px }}"
        ),
    );
    assert_rect(l.rect("f"), rect(100.0, 116.0, 10.0, 10.0));
}

#[test]
fn hit_testing_finds_absolute_boxes() {
    let l = lay(
        r#"<div id=cb><p id=p>text text</p><div id=a></div></div>"#,
        &format!(
            "{RESET} #cb {{ position: relative }} \
             #a {{ position: absolute; left: 0; top: 0; width: 30px; height: 30px; z-index: 1 }}"
        ),
    );
    let hit = l.tree.hit_test(Point::new(5.0, 5.0)).expect("hit");
    assert_eq!(hit.node, Some(l.node("a")));
}
