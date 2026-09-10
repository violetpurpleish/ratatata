use super::*;

#[test]
fn missing_image_extension_arg_opens_a_text_buffer_not_a_preview() {
    let dir = scratch("missingimg");
    let file = dir.join("new.png");
    let app = new_app(dir, Some(file.clone())).unwrap();
    assert!(app.image.is_none());
    assert_eq!(app.buffer.path.as_ref(), Some(&file));
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(!file.exists());
}

#[test]
fn ctrl_n_while_previewing_starts_a_new_buffer() {
    let dir = scratch("newn4");
    write_test_png(&dir.join("pic.png"));
    let mut app = new_app(dir.clone(), Some(dir.join("pic.png"))).unwrap();
    assert!(app.image.is_some());

    app.handle_key(ctrl('n'));
    assert!(app.image.is_none());
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(!app.buffer.dirty);
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn ctrl_r_reloads_the_image_preview() {
    let dir = scratch("reloadimg");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
    assert_eq!(app.image.as_ref().unwrap().pixels, (4, 2));

    let mut img = image::RgbImage::new(2, 3);
    for x in 0..2 {
        for y in 0..3 {
            img.put_pixel(x, y, image::Rgb([x as u8 * 100, y as u8 * 80, 0]));
        }
    }
    img.save(&file).unwrap();
    app.handle_key(ctrl('r'));
    let preview = app.image.as_ref().unwrap();
    assert_eq!(preview.pixels, (2, 3));
    assert_eq!(preview.path, file);
}

#[test]
fn top_bar_in_image_mode_has_preview_buttons() {
    let dir = scratch("mbtnimg");
    write_test_png(&dir.join("pic.png"));
    let mut app = new_app(dir.clone(), Some(dir.join("pic.png"))).unwrap();
    render_buffer(&mut app);
    let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
    assert_eq!(
        actions,
        vec![
            Shortcut::ClosePreview,
            Shortcut::Reload,
            Shortcut::SwitchFocus,
            Shortcut::Quit
        ]
    );
    assert_eq!(app.topbar_area.height, 1);

    // clicking "close preview" drops back to an empty editor buffer
    click_button(&mut app, Shortcut::ClosePreview);
    assert!(app.image.is_none());
    assert_eq!(app.buffer.lines, vec![""]);

    // a second draw shows the full editing button set again
    render_buffer(&mut app);
    let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
    assert_eq!(actions.len(), 17);
    assert_eq!(actions[0], Shortcut::SwitchFocus);
    assert_eq!(actions[1], Shortcut::ToggleSidebar);
    assert_eq!(*actions.last().unwrap(), Shortcut::Quit);
    assert!(actions.contains(&Shortcut::SelectAll));
    assert!(actions.contains(&Shortcut::Replace));
    assert!(actions.contains(&Shortcut::GoToLine));
    assert!(actions.contains(&Shortcut::ToggleHidden));
}

#[test]
fn image_path_detection() {
    use crate::image_view::is_image_path;
    for name in [
        "a.png",
        "a.PNG",
        "photo.JpEg",
        "a.gif",
        "a.webp",
        "a.bmp",
        "a.jpeg",
        "a.tiff",
        "a.tif",
        "a.qoi",
        "a.Pnm",
    ] {
        assert!(is_image_path(std::path::Path::new(&name)), "{name}");
    }
    for name in ["a.txt", "a", "a.png.bak", "dir"] {
        assert!(!is_image_path(std::path::Path::new(name)), "{name}");
    }
}

#[test]
fn opening_image_arg_starts_a_preview() {
    let dir = scratch("imgarg");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    assert!(app.image.is_some());
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(app.buffer.path.is_none());
    assert_eq!(app.focus, Focus::Editor);
    let rows = render(&mut app);
    // status bar advertises the preview: tag, dimensions, and hint
    assert!(row_contains(&rows, "IMAGE"));
    assert!(row_contains(&rows, "[4x2]"));
    assert!(row_contains(&rows, "Esc close preview"));
}

#[test]
fn image_preview_title_uses_image_path() {
    let dir = scratch("imgtitle");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    let rows = render(&mut app);
    assert!(row_contains(&rows, "pic.png"));
    assert!(!row_contains(&rows, "untitled"));
}

#[test]
fn updating_image_metrics_invalidates_protocol_without_redecoding() {
    let dir = scratch("imgmetrics");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    let (pixels, protocol_was_built) = {
        let preview = app.image.as_ref().unwrap();
        (preview.pixels, preview.has_cached_protocol())
    };
    assert!(protocol_was_built);

    app.update_image_metrics(Picker::halfblocks(), FontSize::new(4, 8));
    let preview = app.image.as_ref().unwrap();
    assert_eq!(preview.pixels, pixels);
    assert!(!preview.has_cached_protocol());

    // The next draw rebuilds the protocol from the existing decoded image.
    render_buffer(&mut app);
    assert!(app.image.as_ref().unwrap().has_cached_protocol());
}

#[test]
fn preview_renders_halfblock_pixels() {
    let dir = scratch("imgdraw");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    // the editor pane starts at column 28 (sidebar width); the 4x2 px
    // image is one cell at the assumed 10x20 font. The half-blocks
    // renderer places a colored block character there — the exact
    // color is image-rs's resize math (aspect-fit + triangle filter),
    // so assert the structure: a half-block, colored, with a red/blue
    // blend that contains no green.
    let cell = buf.cell((29, 2)).unwrap();
    assert!(matches!(cell.symbol(), "▀" | "▄"), "{:?}", cell.symbol());
    let fg = cell.style().fg;
    let Some(Color::Rgb(r, g, b)) = fg else {
        panic!("expected an RGB foreground, got {fg:?}");
    };
    assert!(r > 0 && b > 0 && g == 0, "got rgb({r},{g},{b})");
}

#[test]
fn ansi16_halfblock_preview_does_not_emit_rgb() {
    let dir = scratch("img-ansi16");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    app.set_color_support(ColorSupport::Ansi16);
    let buf = render_buffer(&mut app);
    let cell = buf.cell((29, 2)).unwrap();
    assert!(matches!(cell.symbol(), "▀" | "▄"));
    assert_supported_colors(&buf, ColorSupport::Ansi16);
}

#[test]
fn halfblock_color_depth_survives_cached_frames_and_resize() {
    let dir = scratch("img-color-depth");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    let original = render_buffer(&mut app).cell((29, 2)).unwrap().clone();
    assert!(matches!(original.fg, Color::Rgb(..)));
    for support in [ColorSupport::Indexed256, ColorSupport::Ansi16] {
        app.set_color_support(support);
        for _ in 0..2 {
            assert!(app.image.as_ref().unwrap().has_cached_protocol());
            let buf = render_buffer(&mut app);
            let pixel = buf.cell((29, 2)).unwrap();
            assert_eq!(pixel.symbol(), original.symbol());
            if support == ColorSupport::Indexed256 {
                assert!(matches!(pixel.fg, Color::Indexed(16..=255)));
                assert!(matches!(pixel.bg, Color::Indexed(16..=255)));
            }
            assert_supported_colors(&buf, support);
        }
        render_sized(&mut app, 80, 20);
        assert_supported_colors(&render_buffer(&mut app), support);
    }
    app.set_color_support(ColorSupport::TrueColor);
    assert_eq!(render_buffer(&mut app).cell((29, 2)).unwrap(), &original);
}

#[test]
fn large_preview_is_contained_without_clipping() {
    let dir = scratch("imglarge");
    let file = dir.join("large.png");
    write_large_test_png(&file);
    let mut app = new_app(dir.clone(), Some(file)).unwrap();
    let buf = render_buffer(&mut app);

    // The editor's inner area is columns 29..=138. A wide image is
    // contained by height, so it must not be stretched to the right edge.
    let rendered_x: Vec<u16> = (29..=138)
        .filter(|&x| {
            matches!(
                buf.cell((x, 2)).and_then(|c| c.style().fg),
                Some(Color::Rgb(..))
            )
        })
        .collect();
    assert!(!rendered_x.is_empty());
    assert!(*rendered_x.iter().max().unwrap() < 138);

    // A tall image is contained by height too, and its last visible row
    // is still part of the image rather than being clipped away.
    let medium = dir.join("medium.png");
    let mut medium_img = image::RgbImage::new(1000, 400);
    for y in 0..400 {
        for x in 0..1000 {
            medium_img.put_pixel(x, y, image::Rgb([(x / 4) as u8, (y / 2) as u8, 128]));
        }
    }
    medium_img.save(&medium).unwrap();
    let mut medium_app = new_app(dir.clone(), Some(medium)).unwrap();
    let medium_buf = render_buffer(&mut medium_app);
    assert!(matches!(
        medium_buf.cell((29, 21)).and_then(|c| c.style().fg),
        Some(Color::Rgb(..))
    ));

    let tall = dir.join("tall.png");
    let mut img = image::RgbImage::new(600, 1200);
    for y in 0..1200 {
        for x in 0..600 {
            img.put_pixel(x, y, image::Rgb([(x / 3) as u8, (y / 5) as u8, 128]));
        }
    }
    img.save(&tall).unwrap();
    let mut tall_app = new_app(dir, Some(tall)).unwrap();
    let tall_buf = render_buffer(&mut tall_app);
    assert!(matches!(
        tall_buf.cell((29, 21)).and_then(|c| c.style().fg),
        Some(Color::Rgb(..))
    ));
}

#[test]
fn esc_closes_preview_and_other_keys_are_ignored() {
    let dir = scratch("imgclose");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();

    // typing, saving, and searching do nothing while previewing
    app.handle_key(char_key('a'));
    assert!(app.image.is_some());
    assert_eq!(app.buffer.lines, vec![""]);
    app.handle_key(ctrl('s'));
    assert!(app.save_as_input.is_none());
    app.handle_key(ctrl('f'));
    assert!(app.search.is_none());
    // bracketed paste is ignored too
    app.paste_text("pasted".to_string());
    assert_eq!(app.buffer.lines, vec![""]);

    // Esc closes the preview, then editing works again
    app.handle_key(key(KeyCode::Esc));
    assert!(app.image.is_none());
    app.handle_key(char_key('a'));
    assert_eq!(app.buffer.lines, vec!["a"]);
}

#[test]
fn esc_restores_previous_buffer_and_wrap_after_image_preview() {
    let dir = scratch("imgrestore");
    fs::write(dir.join("notes.txt"), "hello wrap me\n").unwrap();
    write_test_png(&dir.join("logo.png"));
    write_test_png(&dir.join("other.png"));
    let mut app = new_app(dir.clone(), Some(dir.join("notes.txt"))).unwrap();
    render_buffer(&mut app);
    app.handle_key(ctrl('w'));
    assert!(app.buffer.wrap);

    app.handle_key(ctrl('o'));
    app.sidebar.select_name("logo.png");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.image.is_some());
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(app.buffer.path.is_none());

    // opening another image keeps the stashed text buffer
    app.handle_key(ctrl('o'));
    app.sidebar.select_name("other.png");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.image.as_ref().unwrap().path, dir.join("other.png"));

    app.handle_key(key(KeyCode::Esc));
    assert!(app.image.is_none());
    assert_eq!(app.buffer.lines, vec!["hello wrap me", ""]);
    assert_eq!(app.buffer.path, Some(dir.join("notes.txt")));
    assert!(app.buffer.wrap);
    let rows = render(&mut app);
    assert!(row_contains(&rows, "notes.txt"));
    assert!(!row_contains(&rows, "untitled"));
}

#[test]
fn ctrl_o_and_ctrl_q_still_work_while_previewing() {
    let dir = scratch("imghotkeys");
    let file = dir.join("pic.png");
    write_test_png(&file);
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl('o'));
    assert_eq!(app.focus, Focus::Sidebar);
    app.handle_key(ctrl('o'));
    assert_eq!(app.focus, Focus::Editor);
    // clean buffer: Ctrl+Q quits immediately
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
}

#[test]
fn sidebar_navigation_continues_while_preview_is_active() {
    let dir = scratch("imgnavigate");
    write_test_png(&dir.join("pic.png"));
    write_test_png(&dir.join("other.png"));
    fs::write(dir.join("notes.txt"), "text\n").unwrap();
    fs::create_dir(dir.join("nested")).unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("pic.png"))).unwrap();

    app.handle_key(ctrl('o'));
    assert_eq!(app.focus, Focus::Sidebar);
    app.sidebar.select_name("notes.txt");
    let start = app.sidebar.selected;
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.sidebar.selected, start);

    // Directory navigation remains available even though the editor is
    // displaying an image.
    app.sidebar.select_name("nested");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.image.is_some());
    assert_eq!(app.focus, Focus::Sidebar);
    assert_eq!(app.sidebar.dir, dir.join("nested"));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.sidebar.dir, dir);

    // Enter can open another image directly from the active preview.
    app.sidebar.select_name("other.png");
    app.handle_key(key(KeyCode::Enter));
    let preview = app.image.as_ref().expect("image preview");
    assert_eq!(preview.path, dir.join("other.png"));
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn sidebar_opens_images_and_dirty_buffers_block_them() {
    let dir = scratch("imgside");
    fs::write(dir.join("a.txt"), "text\n").unwrap();
    write_test_png(&dir.join("pic.png"));
    let mut app = new_app(dir, None).unwrap();
    // ".." = 0, a.txt = 1, pic.png = 2
    app.sidebar.selected = 2;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.image.is_some());
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.buffer.lines, vec![""]);

    // a dirty buffer blocks opening an image, like any other file
    let dir = scratch("imgside2");
    fs::write(dir.join("a.txt"), "text\n").unwrap();
    write_test_png(&dir.join("pic.png"));
    let mut app = new_app(dir, None).unwrap();
    app.sidebar.selected = 1;
    app.handle_key(key(KeyCode::Enter)); // open a.txt
    app.handle_key(char_key('x')); // dirty it
    app.handle_key(ctrl('o')); // back to the sidebar
    app.sidebar.selected = 2;
    app.handle_key(key(KeyCode::Enter)); // try to open pic.png
    assert!(app.image.is_none());
    assert!(app.buffer.dirty);
}

#[test]
fn corrupt_image_is_an_open_error() {
    let dir = scratch("imgcorrupt");
    let file = dir.join("bad.png");
    fs::write(&file, "definitely not an image").unwrap();
    let picker = Picker::halfblocks();
    let err = match App::new_with_cell_size(
        dir,
        Some(file),
        picker,
        image_view::fallback_logical_cell_size(),
    ) {
        Ok(_) => panic!("expected an error for a corrupt image"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("not a valid image"), "{err}");
}
