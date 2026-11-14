//! Rendering: Direct2D (software DC render target) + DirectWrite into a 32-bit DIB,
//! presented with UpdateLayeredWindow. No D3D device, so the working set stays small.
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

pub fn hex(v: u32) -> Color {
    Color(((v >> 16) & 255) as f32 / 255.0, ((v >> 8) & 255) as f32 / 255.0, (v & 255) as f32 / 255.0, 1.0)
}
impl Color {
    pub fn a(self, a: f32) -> Color {
        Color(self.0, self.1, self.2, self.3 * a)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
pub enum Font {
    Body = 0,
    Bold = 1,
    Small = 2,
    Icon = 3,
    Badge = 4,
}

pub struct Gfx {
    _factory: ID2D1Factory,
    dw: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    fonts: Vec<IDWriteTextFormat>,
    /// (geometry, view width, view height, fixed colour)
    logos: Vec<(ID2D1PathGeometry, f32, f32, Option<u32>)>,
    memdc: HDC,
    dib: HBITMAP,
    oldbmp: HGDIOBJ,
    bits: *mut u8,
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    /// Global opacity multiplier for everything drawn.
    pub alpha: std::cell::Cell<f32>,
    /// The rectangle this frame is limited to, if any.
    partial: std::cell::Cell<Option<[f32; 4]>>,
    /// A clip was pushed for this frame and must be popped before it ends.
    clipped: std::cell::Cell<bool>,
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl Gfx {
    pub fn new(w: i32, h: i32, scale: f32) -> Result<Gfx> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            let memdc = CreateCompatibleDC(None);
            let mut fonts = Vec::new();
            // DM Sans ships inside the exe; if loading it fails we fall back to Segoe UI Variable.
            let dm = dm_sans(&dw);
            let ui_family = if dm.is_some() { w!("DM Sans") } else { w!("Segoe UI Variable Text") };
            // (icon font?, weight, size)
            let specs: [(bool, DWRITE_FONT_WEIGHT, f32); 5] = [
                (false, DWRITE_FONT_WEIGHT_NORMAL, 12.5),
                (false, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0),
                (false, DWRITE_FONT_WEIGHT_NORMAL, 11.0),
                (true, DWRITE_FONT_WEIGHT_NORMAL, 12.0),
                (false, DWRITE_FONT_WEIGHT_BOLD, 10.0),
            ];
            for (icon, weight, size) in specs {
                let f = if icon {
                    dw.CreateTextFormat(w!("Segoe MDL2 Assets"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?
                } else {
                    dw.CreateTextFormat(ui_family, dm.as_ref(), weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?
                };
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                if let Ok(sign) = dw.CreateEllipsisTrimmingSign(&f) {
                    let _ = f.SetTrimming(&trim, &sign);
                }
                fonts.push(f);
            }
            let (dib, bits) = make_dib(memdc, w, h)?;
            let oldbmp = SelectObject(memdc, dib.into());
            rt.BindDC(memdc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
            let logos = crate::logos::ALL.iter().filter_map(|d| build_logo(&factory, d)).collect();
            let g = Gfx { _factory: factory, dw, rt, brush, fonts, logos, memdc, dib, oldbmp, bits, w, h, scale, alpha: std::cell::Cell::new(1.0), partial: std::cell::Cell::new(None), clipped: std::cell::Cell::new(false) };
            g.rt.SetDpi(96.0 * scale, 96.0 * scale);
            g.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(g)
        }
    }

    /// Recreate the backing bitmap for a new pixel size / DPI scale.
    pub fn resize(&mut self, w: i32, h: i32, scale: f32) {
        if w == self.w && h == self.h && scale == self.scale {
            return;
        }
        unsafe {
            if let Ok((dib, bits)) = make_dib(self.memdc, w, h) {
                SelectObject(self.memdc, dib.into());
                let _ = DeleteObject(self.dib.into());
                self.dib = dib;
                self.bits = bits;
                self.w = w;
                self.h = h;
                self.scale = scale;
                let _ = self.rt.BindDC(self.memdc, &RECT { left: 0, top: 0, right: w, bottom: h });
                self.rt.SetDpi(96.0 * scale, 96.0 * scale);
            }
        }
    }

    /// Start a frame. With `only`, drawing (and the clear) is limited to that rectangle
    /// (DIPs): everything outside keeps last frame's pixels and costs nothing to rasterize.
    pub fn begin(&self, only: Option<[f32; 4]>) {
        unsafe {
            self.rt.BeginDraw();
            if let Some([x0, y0, x1, y1]) = only {
                self.rt.PushAxisAlignedClip(&D2D_RECT_F { left: x0, top: y0, right: x1, bottom: y1 }, D2D1_ANTIALIAS_MODE_ALIASED);
            }
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
        self.partial.set(only);
        self.clipped.set(only.is_some());
        self.alpha.set(1.0);
    }

    /// Start a frame that paints over what is already in the bitmap (nothing is cleared).
    /// Only `dirty` (DIPs) is sent to the compositor when the frame is presented.
    pub fn begin_over(&self, dirty: [f32; 4]) {
        unsafe {
            self.rt.BeginDraw();
        }
        self.partial.set(Some(dirty));
        self.clipped.set(false);
        self.alpha.set(1.0);
    }

    /// Finish drawing and push the bitmap to the layered window at screen position (x, y).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) {
        unsafe {
            let partial = self.partial.take();
            if self.clipped.replace(false) {
                self.rt.PopAxisAlignedClip();
            }
            let _ = self.rt.EndDraw(None, None);
            let dst = POINT { x, y };
            let size = SIZE { cx: self.w, cy: self.h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            // tell the compositor which pixels changed, so it does not re-upload the rest
            let s = self.scale;
            let dirty = partial.map(|[x0, y0, x1, y1]| RECT { left: ((x0 * s).floor() as i32).max(0), top: ((y0 * s).floor() as i32).max(0), right: ((x1 * s).ceil() as i32).min(self.w), bottom: ((y1 * s).ceil() as i32).min(self.h) });
            let info = UPDATELAYEREDWINDOWINFO {
                cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                hdcDst: HDC::default(),
                pptDst: &dst,
                psize: &size,
                hdcSrc: self.memdc,
                pptSrc: &src,
                crKey: COLORREF(0),
                pblend: &blend,
                dwFlags: ULW_ALPHA,
                prcDirty: dirty.as_ref().map(|r| r as *const RECT).unwrap_or(std::ptr::null()),
            };
            let _ = UpdateLayeredWindowIndirect(hwnd, &info);
        }
    }

    /// Finish drawing without presenting (snapshot mode).
    pub fn end(&self) {
        unsafe {
            self.partial.set(None);
            if self.clipped.replace(false) {
                self.rt.PopAxisAlignedClip();
            }
            let _ = self.rt.EndDraw(None, None);
        }
    }

    /// Raw premultiplied BGRA pixels, top-down.
    pub fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize) }
    }

    fn set(&self, c: Color) {
        unsafe {
            self.brush.SetColor(&D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 * self.alpha.get() });
        }
    }

    pub fn fill_rr(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        self.set(c);
        unsafe {
            self.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r }, &self.brush);
        }
    }

    /// Rounded rectangle filled with a vertical gradient (top colour to bottom colour).
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rr_grad(&self, x: f32, y: f32, w: f32, h: f32, r: f32, top: Color, bottom: Color) {
        let al = self.alpha.get();
        let stop = |p: f32, c: Color| D2D1_GRADIENT_STOP { position: p, color: D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 * al } };
        unsafe {
            let Ok(stops) = self.rt.CreateGradientStopCollection(&[stop(0.0, top), stop(1.0, bottom)], D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: x, Y: y }, endPoint: Vector2 { X: x, Y: y + h } };
            if let Ok(b) = self.rt.CreateLinearGradientBrush(&props, None, &stops) {
                self.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r }, &b);
            }
        }
    }

    /// A notch: a shape that grows out of a screen edge, with rounded outer corners and
    /// small concave fillets ("ears") where it meets the edge. `side` is 0 top, 1 bottom,
    /// 2 left, 3 right: the side of the rectangle that touches the screen edge.
    #[allow(clippy::too_many_arguments)]
    pub fn notch(&self, x: f32, y: f32, w: f32, h: f32, r: f32, ear: f32, side: u8, c: Color) {
        // local frame: u runs along the edge, v is the distance out from it
        let (len, depth) = if side < 2 { (w, h) } else { (h, w) };
        let map = |u: f32, v: f32| match side {
            0 => Vector2 { X: x + u, Y: y + v },
            1 => Vector2 { X: x + u, Y: y + h - v },
            2 => Vector2 { X: x + v, Y: y + u },
            _ => Vector2 { X: x + w - v, Y: y + u },
        };
        let ear = ear.min(depth / 2.0).max(0.0);
        let r = r.min(len / 2.0).min(depth - ear).max(0.0);
        const K: f32 = 0.5523; // quarter circle as a cubic
        self.set(c);
        unsafe {
            let Ok(geo) = self._factory.CreatePathGeometry() else { return };
            let Ok(sink) = geo.Open() else { return };
            // quarter turn from the current point to (bu, bv) around the corner (cu, cv)
            let quarter = |au: f32, av: f32, cu: f32, cv: f32, bu: f32, bv: f32| {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: map(au + K * (cu - au), av + K * (cv - av)), point2: map(bu + K * (cu - bu), bv + K * (cv - bv)), point3: map(bu, bv) });
            };
            // start one pixel past the edge so no seam shows between notch and screen edge
            sink.BeginFigure(map(-ear, -1.0), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(map(-ear, 0.0));
            quarter(-ear, 0.0, 0.0, 0.0, 0.0, ear);
            sink.AddLine(map(0.0, depth - r));
            quarter(0.0, depth - r, 0.0, depth, r, depth);
            sink.AddLine(map(len - r, depth));
            quarter(len - r, depth, len, depth, len, depth - r);
            sink.AddLine(map(len, ear));
            quarter(len, ear, len, 0.0, len + ear, 0.0);
            sink.AddLine(map(len + ear, -1.0));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                self.rt.FillGeometry(&geo, &self.brush, None);
            }
        }
    }

    /// Draw agent logo `i` scaled to fit a `size` square at (x, y).
    pub fn logo(&self, i: usize, x: f32, y: f32, size: f32, fallback: Color) {
        let Some((geo, vw, vh, color)) = self.logos.get(i) else { return };
        let s = size / vw.max(*vh);
        let (dx, dy) = (x + (size - vw * s) / 2.0, y + (size - vh * s) / 2.0);
        self.set(color.map(hex).unwrap_or(fallback));
        unsafe {
            self.rt.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: dx, M32: dy });
            self.rt.FillGeometry(geo, &self.brush, None);
            self.rt.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 });
        }
    }

    pub fn stroke_rr(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color, width: f32) {
        self.set(c);
        unsafe {
            self.rt.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r }, &self.brush, width, None);
        }
    }

    pub fn dot(&self, cx: f32, cy: f32, r: f32, c: Color) {
        self.set(c);
        unsafe {
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, &self.brush);
        }
    }

    pub fn ring(&self, cx: f32, cy: f32, r: f32, c: Color, width: f32) {
        self.set(c);
        unsafe {
            self.rt.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, &self.brush, width, None);
        }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        self.set(c);
        unsafe {
            self.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, &self.brush, width, None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text(&self, s: &str, font: Font, x: f32, y: f32, w: f32, h: f32, align: Align, c: Color) {
        if s.is_empty() || w <= 0.0 {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let f = &self.fonts[font as usize];
        self.set(c);
        unsafe {
            let _ = f.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            self.rt.DrawText(&wide, f, &rect(x, y, w, h), &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// Width of a single line of text in DIPs.
    pub fn measure(&self, s: &str, font: Font) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let f = &self.fonts[font as usize];
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            match self.dw.CreateTextLayout(&wide, f, 4096.0, 100.0) {
                Ok(l) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    if l.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
                }
                Err(_) => 0.0,
            }
        }
    }

    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe {
            self.rt.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
    }
    pub fn unclip(&self) {
        unsafe {
            self.rt.PopAxisAlignedClip();
        }
    }
}

impl Drop for Gfx {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memdc, self.oldbmp);
            let _ = DeleteObject(self.dib.into());
            let _ = DeleteDC(self.memdc);
        }
    }
}

/// Turn a logo's SVG paths into one Direct2D geometry (built once at startup).
unsafe fn build_logo(factory: &ID2D1Factory, def: &crate::logos::LogoDef) -> Option<(ID2D1PathGeometry, f32, f32, Option<u32>)> {
    use crate::svgpath::Seg;
    unsafe {
        let geo = factory.CreatePathGeometry().ok()?;
        let sink = geo.Open().ok()?;
        sink.SetFillMode(if def.even_odd { D2D1_FILL_MODE_ALTERNATE } else { D2D1_FILL_MODE_WINDING });
        let v = |x: f32, y: f32| Vector2 { X: x, Y: y };
        for d in def.paths {
            let mut open = false;
            let (mut cur, mut start) = ((0.0, 0.0), (0.0, 0.0));
            for seg in crate::svgpath::parse(d) {
                match seg {
                    Seg::Move(x, y) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        sink.BeginFigure(v(x, y), D2D1_FIGURE_BEGIN_FILLED);
                        open = true;
                        cur = (x, y);
                        start = cur;
                        continue;
                    }
                    Seg::Close => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        }
                        open = false;
                        cur = start;
                        continue;
                    }
                    _ => {}
                }
                if !open {
                    // drawing after a closepath starts a new figure at the old start point
                    sink.BeginFigure(v(cur.0, cur.1), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
                match seg {
                    Seg::Line(x, y) => {
                        sink.AddLine(v(x, y));
                        cur = (x, y);
                    }
                    Seg::Cubic(x1, y1, x2, y2, x, y) => {
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v(x1, y1), point2: v(x2, y2), point3: v(x, y) });
                        cur = (x, y);
                    }
                    Seg::Quad(x1, y1, x, y) => {
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: v(x1, y1), point2: v(x, y) });
                        cur = (x, y);
                    }
                    Seg::Arc { rx, ry, rot, large, sweep, x, y } => {
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: v(x, y),
                            size: D2D_SIZE_F { width: rx, height: ry },
                            rotationAngle: rot,
                            sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                            arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        });
                        cur = (x, y);
                    }
                    _ => {}
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
        }
        sink.Close().ok()?;
        Some((geo, def.view.0, def.view.1, def.color))
    }
}

static DM_SANS: &[u8] = include_bytes!("../assets/DMSans.ttf");

/// Build a private font collection holding the embedded DM Sans (no install, no temp file).
unsafe fn dm_sans(dw: &IDWriteFactory) -> Option<IDWriteFontCollection> {
    unsafe {
        let f5: IDWriteFactory5 = dw.cast().ok()?;
        let loader = f5.CreateInMemoryFontFileLoader().ok()?;
        f5.RegisterFontFileLoader(&loader).ok()?;
        let file = loader.CreateInMemoryFontFileReference(&f5, DM_SANS.as_ptr() as *const _, DM_SANS.len() as u32, None).ok()?;
        let builder = f5.CreateFontSetBuilder().ok()?;
        builder.AddFontFile(&file).ok()?;
        let set = builder.CreateFontSet().ok()?;
        let coll = f5.CreateFontCollectionFromFontSet(&set).ok()?;
        coll.cast::<IDWriteFontCollection>().ok()
    }
}

unsafe fn make_dib(dc: HDC, w: i32, h: i32) -> Result<(HBITMAP, *mut u8)> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let dib = unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };
    Ok((dib, bits as *mut u8))
}
