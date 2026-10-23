use crate::app::{SwitchAppWindowsState, SwitchAppsState};
use crate::config::SwitchAppsColors;
use crate::utils::{check_error, get_moinitor_rect};

use anyhow::{anyhow, Context, Result};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use windows::core::{w, Interface, PCWSTR};
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::System::DispatcherQueueController;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionCapabilities, CompositionColorBrush, CompositionDrawingSurface,
    CompositionGraphicsDevice, Compositor, ContainerVisual, DropShadow,
    CompositionRoundedRectangleGeometry, SpriteVisual,
};
use windows::Win32::{
    Foundation::{HMODULE, HWND, POINT, SIZE},
    Graphics::{
        Direct2D::{
            Common::{
                D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
            },
            D2D1CreateDevice, ID2D1Device, ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE,
            D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PRIMITIVE_BLEND_COPY,
        },
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
        Direct3D11::{
            D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        },
        Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice},
        Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDIBits,
            GetObjectW, ReleaseDC, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            DIB_RGB_COLORS, HDC,
        },
        GdiPlus::{
            FillModeAlternate, FontStyleBold, FontStyleRegular, GdipAddPathArc, GdipClosePathFigure,
            GdipCreateBitmapFromScan0, GdipCreateFont, GdipCreateFontFamilyFromName,
            GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipCreateStringFormat,
            GdipDeleteBrush, GdipDeleteFont, GdipDeleteFontFamily, GdipDeleteGraphics,
            GdipDeletePath, GdipDeletePen, GdipDeleteStringFormat, GdipDisposeImage,
            GdipDrawImageRectI, GdipDrawPath, GdipDrawString, GdipFillPath,
            GdipGetImageGraphicsContext, GdipGraphicsClear, GdipMeasureString,
            GdipSetInterpolationMode, GdipSetPixelOffsetMode, GdipSetSmoothingMode,
            GdipSetStringFormatAlign, GdipSetStringFormatFlags, GdipSetStringFormatLineAlign,
            GdipSetStringFormatTrimming, GdipSetTextRenderingHint, GdiplusShutdown, GdiplusStartup,
            GdiplusStartupInput, GpBitmap, GpBrush, GpFont, GpFontFamily, GpGraphics, GpImage,
            GpPath, GpPen, GpSolidFill, GpStringFormat, InterpolationModeHighQualityBicubic,
            PixelOffsetModeHighQuality, RectF, SmoothingModeAntiAlias, StringAlignmentCenter,
            StringAlignmentNear, StringFormatFlagsNoWrap, StringTrimmingEllipsisCharacter,
            TextRenderingHintAntiAlias, Unit, UnitPixel,
        },
    },
    System::WinRT::{
        Composition::{
            ICompositionCapabilitiesInteropFactory, ICompositionDrawingSurfaceInterop,
            ICompositorDesktopInterop, ICompositorInterop,
        },
        CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
        DQTYPE_THREAD_CURRENT,
    },
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::SetFocus,
        WindowsAndMessaging::{
            GetCursorPos, GetIconInfo, SetWindowPos, ShowWindow, HICON, HWND_TOPMOST, ICONINFO,
            SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNA,
        },
    },
};
use windows_numerics::{Vector2, Vector3};

pub const ICON_SIZE_BASE: i32 = 64;
/// Padding around each icon, i.e. the margin between apps in the list.
pub const ICON_BORDER_SIZE_BASE: i32 = 14;

// Vertical app-windows popup metrics, in px @ 96dpi (scaled by DPI at paint time).
/// Icon size on each row.
pub const ROW_ICON_SIZE_BASE: i32 = 24;
/// Height of each window row.
pub const ROW_HEIGHT_BASE: i32 = 36;
/// Horizontal padding around the icon / between icon and text / trailing the text.
pub const ROW_TEXT_PAD_BASE: i32 = 10;
/// Margin between the popup pane edge and the row list (smaller than the app row's border).
pub const POPUP_BORDER_BASE: i32 = 6;
/// Vertical gap above/below the selection highlight within a row.
const ROW_SEL_INSET_BASE: i32 = 2;
/// Padding between the app icon and the selection highlight around it (px @ 96dpi).
/// Smaller than `ICON_BORDER_SIZE_BASE` so the cursor hugs the icon.
const APP_CURSOR_PADDING_BASE: i32 = 6;
/// Vertical margin between the app-switcher pane edge and the icon row (px @ 96dpi). Kept
/// small so the pane is compact and the name label sits near the cursor.
const APP_BORDER_SIZE_BASE: i32 = 8;
/// Horizontal margin between the app-switcher pane edge and the icon row (px @ 96dpi).
const APP_H_BORDER_SIZE_BASE: i32 = 16;
/// Height of the app-switcher band that shows the selected app's name (px @ 96dpi).
const APP_LABEL_HEIGHT_BASE: i32 = 14;
/// Font em-size for the app-switcher name label (smaller than the window-list font).
const APP_LABEL_FONT_BASE: f32 = 11.5;
/// Font em-size (UnitPixel) for the window titles.
const FONT_SIZE_BASE: f32 = 14.0;
/// Min / max popup width clamp, in px @ 96dpi.
const POPUP_MIN_WIDTH_BASE: i32 = 280;
const POPUP_MAX_WIDTH_BASE: i32 = 680;
/// Title text color (opaque dark grey).
const TITLE_TEXT_COLOR: u32 = 0xff20_2020;

// The pane background tint, bevel border (light/dark), and selection colors are
// configurable via the .ini; see `SwitchAppsColors` and its defaults in config.rs.
/// Border thickness in px @ 96dpi (scaled by DPI).
const BORDER_WIDTH_BASE: f32 = 1.0;
/// How far the border softens/blurs inward, in px @ 96dpi (scaled by DPI).
const BORDER_BLUR_BASE: f32 = 2.5;

/// Corner radius / shadow geometry, in px @ 96dpi (scaled by DPI at paint time).
const CORNER_RADIUS_BASE: f32 = 8.0;
const SHADOW_BLUR_BASE: f32 = 18.0;
const SHADOW_OFFSET_Y_BASE: f32 = 3.0;
const SHADOW_OPACITY: f32 = 0.5;

/// 32bpp premultiplied ARGB, the format the GDI+ strip rasterizer produces.
const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000e200b;

/// Shared Windows.UI.Composition overlay host: a host-backdrop acrylic pane with rounded
/// corners and a real composition drop shadow, hosted on the app window. Both the Alt+Tab
/// (horizontal) and Alt+Ctrl+Tab (vertical) painters draw into this single surface — only
/// one popup is ever shown at a time — by handing `render` a closure that rasterizes their
/// content with GDI+ into a premultiplied DIB which is then uploaded to the surface.
pub struct Overlay {
    hwnd: HWND,
    hdc_screen: HDC,
    token: usize, // GdiplusStartup token
    show: bool,
    effects_supported: bool,
    last_size: (i32, i32),
    colors: SwitchAppsColors,

    // Composition visual tree (resized per paint). The compositor, target and graphics
    // device are held only to keep the tree/surface alive for the renderer's lifetime.
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _graphics_device: CompositionGraphicsDevice,
    drawing_surface: CompositionDrawingSurface,
    root: ContainerVisual,
    backdrop: SpriteVisual,
    pane: SpriteVisual,
    content: SpriteVisual,
    rounded_geo: CompositionRoundedRectangleGeometry,
    drop_shadow: DropShadow,
    tint_brush: CompositionColorBrush,

    // Kept alive for the lifetime of the renderer (declared so they drop last).
    _d3d_device: ID3D11Device,
    _d2d_device: ID2D1Device,
    _dispatcher_controller: DispatcherQueueController,
}

impl Overlay {
    pub fn new(hwnd: HWND, colors: SwitchAppsColors) -> Result<Self> {
        // A DispatcherQueue on this (message-loop) thread is required before the
        // Compositor; DQTAT_COM_STA initializes the apartment as STA.
        let dispatcher_controller = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
        }
        .context("Failed to create dispatcher queue controller")?;

        let compositor = Compositor::new().context("Failed to create compositor")?;

        // D3D11 -> DXGI -> D2D device, then a composition graphics device.
        let d3d_device = create_d3d_device().context("Failed to create D3D11 device")?;
        let dxgi: IDXGIDevice = d3d_device.cast()?;
        let d2d_device = unsafe { D2D1CreateDevice(&dxgi, None) }?;
        let comp_interop: ICompositorInterop = compositor.cast()?;
        let graphics_device = unsafe { comp_interop.CreateGraphicsDevice(&d2d_device) }?;

        // Host the visual tree on the window.
        let desktop_interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = unsafe { desktop_interop.CreateDesktopWindowTarget(hwnd, true) }?;

        // Build the tree:  root -> [ backdrop (blur), pane (tint + shadow) -> content (icons) ]
        let root = compositor.CreateContainerVisual()?;
        let backdrop = compositor.CreateSpriteVisual()?;
        let pane = compositor.CreateSpriteVisual()?;
        let content = compositor.CreateSpriteVisual()?;

        let backdrop_brush = compositor.CreateHostBackdropBrush()?;
        backdrop.SetBrush(&backdrop_brush)?;

        let tint_brush = compositor.CreateColorBrushWithColor(argb_color(colors.background))?;
        pane.SetBrush(&tint_brush)?;

        let drawing_surface = graphics_device.CreateDrawingSurface(
            Size {
                Width: 1.0,
                Height: 1.0,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let surface_brush = compositor.CreateSurfaceBrushWithSurface(&drawing_surface)?;
        content.SetBrush(&surface_brush)?;

        // Rounded clip applied to both the blur and the pane (host backdrop can ignore
        // a parent clip, so clip each sprite directly).
        let rounded_geo = compositor.CreateRoundedRectangleGeometry()?;
        backdrop.SetClip(&compositor.CreateGeometricClipWithGeometry(&rounded_geo)?)?;
        pane.SetClip(&compositor.CreateGeometricClipWithGeometry(&rounded_geo)?)?;

        // Rounded composition drop shadow, derived from the pane's tinted silhouette.
        let drop_shadow = compositor.CreateDropShadow()?;
        drop_shadow.SetColor(argb_color(0xff000000))?;
        drop_shadow.SetOpacity(SHADOW_OPACITY)?;
        pane.SetShadow(&drop_shadow)?;

        pane.Children()?.InsertAtTop(&content)?;
        let root_children = root.Children()?;
        root_children.InsertAtBottom(&backdrop)?;
        root_children.InsertAtTop(&pane)?;
        root.SetIsVisible(false)?;
        target.SetRoot(&root)?;

        // Are composition effects available (transparency on, not battery saver, ...)?
        // Diagnostic: force on to test the host-backdrop blur directly.
        let _ = query_effects_supported(hwnd);
        let effects_supported = true;

        // Acquire the GDI resources last: they are freed by Self's Drop, which only
        // runs once Self is constructed, so any `?` failure above would otherwise leak
        // them. GDI+ rasterizes the icon strip; the screen DC backs the per-paint DIB.
        // Nothing between here and `Ok(Self)` is fallible, so neither can leak.
        let startup_input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let mut token: usize = 0;
        check_error(|| unsafe { GdiplusStartup(&mut token, &startup_input, std::ptr::null_mut()) })
            .context("Failed to initialize GDI+")?;
        let hdc_screen = unsafe { GetDC(None) };

        Ok(Self {
            hwnd,
            hdc_screen,
            token,
            show: false,
            effects_supported,
            last_size: (0, 0),
            colors,
            _compositor: compositor,
            _target: target,
            _graphics_device: graphics_device,
            drawing_surface,
            root,
            backdrop,
            pane,
            content,
            rounded_geo,
            drop_shadow,
            tint_brush,
            _d3d_device: d3d_device,
            _d2d_device: d2d_device,
            _dispatcher_controller: dispatcher_controller,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn colors(&self) -> SwitchAppsColors {
        self.colors
    }

    /// Positions/sizes the host window for a `width`x`height` pane at `(x, y)`, resizes the
    /// composition tree, rasterizes the caller's content via `draw` into a premultiplied DIB,
    /// uploads it, and shows the window. `draw` receives a GDI+ graphics context whose origin
    /// is the pane's top-left corner; it should paint onto a fully transparent background.
    pub fn render<F>(&mut self, x: i32, y: i32, width: i32, height: i32, dpi_scale: f64, draw: F)
    where
        F: FnOnce(*mut GpGraphics),
    {
        // Shadow margin: room around the pane for the blurred shadow.
        let blur = (SHADOW_BLUR_BASE * dpi_scale as f32).round();
        let margin = (blur + 12.0 * dpi_scale as f32).round() as i32;

        // Position + size the window (was UpdateLayeredWindow's job).
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x - margin,
                y - margin,
                width + 2 * margin,
                height + 2 * margin,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }

        if let Err(err) = self.layout(width, height, margin, dpi_scale) {
            error!("composition layout failed: {err}");
        }
        if let Err(err) = unsafe { self.rasterize_and_upload(width, height, draw) } {
            error!("strip upload failed: {err}");
        }

        if !self.show {
            let _ = self.root.SetIsVisible(true);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                let _ = SetFocus(Some(self.hwnd));
            }
            self.show = true;
        }
    }

    /// Returns the shadow margin (px) for a given DPI — the gap the popup leaves around the
    /// pane so its drop shadow isn't clipped. Painters need it to size their layout.
    pub fn shadow_margin(dpi_scale: f64) -> i32 {
        let blur = (SHADOW_BLUR_BASE * dpi_scale as f32).round();
        (blur + 12.0 * dpi_scale as f32).round() as i32
    }

    /// Resizes the visual tree to the current pane size (only when it changed).
    fn layout(&mut self, width: i32, height: i32, margin: i32, dpi_scale: f64) -> Result<()> {
        if self.last_size == (width, height) {
            return Ok(());
        }
        let w = width as f32;
        let h = height as f32;
        let m = margin as f32;

        self.root.SetSize(Vector2 {
            X: w + 2.0 * m,
            Y: h + 2.0 * m,
        })?;
        let pane_size = Vector2 { X: w, Y: h };
        let pane_offset = Vector3 { X: m, Y: m, Z: 0.0 };
        self.backdrop.SetSize(pane_size)?;
        self.backdrop.SetOffset(pane_offset)?;
        self.pane.SetSize(pane_size)?;
        self.pane.SetOffset(pane_offset)?;
        self.content.SetSize(pane_size)?;

        self.rounded_geo.SetSize(pane_size)?;
        let r = CORNER_RADIUS_BASE * dpi_scale as f32;
        self.rounded_geo.SetCornerRadius(Vector2 { X: r, Y: r })?;

        self.drop_shadow
            .SetBlurRadius(SHADOW_BLUR_BASE * dpi_scale as f32)?;
        self.drop_shadow.SetOffset(Vector3 {
            X: 0.0,
            Y: SHADOW_OFFSET_Y_BASE * dpi_scale as f32,
            Z: 0.0,
        })?;

        // Host-backdrop blur renders opaque black on Win32 desktop windows, so the blur
        // layer stays hidden; the pane is a translucent frosted tint over the desktop.
        let _ = self.effects_supported;
        self.backdrop.SetIsVisible(false)?;
        self.tint_brush.SetColor(argb_color(self.colors.background))?;

        let si: ICompositionDrawingSurfaceInterop = self.drawing_surface.cast()?;
        unsafe { si.Resize(SIZE { cx: width, cy: height })? };
        self.last_size = (width, height);
        Ok(())
    }

    /// Creates a premultiplied top-down 32bpp DIB + GDI+ graphics context of the given size,
    /// clears it transparent, runs `draw` to render content, then uploads it to the surface.
    unsafe fn rasterize_and_upload<F>(&self, width: i32, height: i32, draw: F) -> Result<()>
    where
        F: FnOnce(*mut GpGraphics),
    {
        unsafe {
            let hdc_mem = CreateCompatibleDC(Some(self.hdc_screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap_mem =
                match CreateDIBSection(Some(self.hdc_screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = DeleteDC(hdc_mem);
                        return Err(anyhow!("CreateDIBSection failed"));
                    }
                };
            let old_bitmap = SelectObject(hdc_mem, bitmap_mem.into());

            let mut bitmap_ptr: *mut GpBitmap = std::ptr::null_mut();
            GdipCreateBitmapFromScan0(
                width,
                height,
                width * 4,
                PIXEL_FORMAT_32BPP_PARGB,
                Some(bits as *const u8),
                &mut bitmap_ptr,
            );
            let image_ptr: *mut GpImage = bitmap_ptr as *mut GpImage;
            let mut g: *mut GpGraphics = std::ptr::null_mut();
            GdipGetImageGraphicsContext(image_ptr, &mut g);
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);

            // Transparent background: the tint + blur come from composition layers.
            GdipGraphicsClear(g, 0x00000000);

            draw(g);

            GdipDeleteGraphics(g);
            GdipDisposeImage(image_ptr);

            // Upload the DIB into the composition drawing surface via D2D.
            let upload = self.upload_surface(bits, width, height);

            SelectObject(hdc_mem, old_bitmap);
            let _ = DeleteObject(bitmap_mem.into());
            let _ = DeleteDC(hdc_mem);
            upload
        }
    }

    unsafe fn upload_surface(&self, bits: *mut c_void, width: i32, height: i32) -> Result<()> {
        unsafe {
            let si: ICompositionDrawingSurfaceInterop = self.drawing_surface.cast()?;
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = si.BeginDraw(None, &mut offset)?;

            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let bitmap = dc.CreateBitmap(
                D2D_SIZE_U {
                    width: width as u32,
                    height: height as u32,
                },
                Some(bits as *const c_void),
                (width * 4) as u32,
                &props,
            )?;

            // COPY blend so transparent source pixels overwrite stale surface content.
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
            let dest = D2D_RECT_F {
                left: offset.x as f32,
                top: offset.y as f32,
                right: (offset.x + width) as f32,
                bottom: (offset.y + height) as f32,
            };
            dc.DrawBitmap(
                &bitmap,
                Some(&dest),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            si.EndDraw()?;
        }
        Ok(())
    }

    /// Hides the overlay window (ends a popup).
    pub fn hide(&mut self) {
        let _ = self.root.SetIsVisible(false);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.show = false;
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            ReleaseDC(None, self.hdc_screen);
            GdiplusShutdown(self.token);
        }
        // Composition COM objects + dispatcher controller drop via field order.
    }
}

/// The Alt+Tab app switcher: a horizontal row of app icons with a selection highlight and,
/// in a band beneath the row, the selected app's name.
pub struct AppsPainter {
    label: TextRenderer,
}

impl AppsPainter {
    /// Must be constructed AFTER `Overlay::new` (which initializes GDI+).
    pub fn new() -> Self {
        Self {
            label: TextRenderer::new(true, true),
        }
    }

    pub fn paint(&mut self, overlay: &mut Overlay, state: &SwitchAppsState) {
        let dpi_scale = get_dpi_scale(overlay.hwnd());
        let icon_size_max = (ICON_SIZE_BASE as f64 * dpi_scale) as i32;
        let border_h = (APP_H_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let border_v = (APP_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let icon_border = (ICON_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let cursor_pad = (APP_CURSOR_PADDING_BASE as f64 * dpi_scale) as i32;
        let label_height = (APP_LABEL_HEIGHT_BASE as f64 * dpi_scale) as i32;
        self.label
            .ensure_font(APP_LABEL_FONT_BASE * dpi_scale as f32, dpi_scale);

        let Coordinate {
            x,
            y,
            width,
            height,
            icon_size,
            item_size,
        } = Coordinate::new(
            state.apps.len() as i32,
            icon_size_max,
            border_h,
            border_v,
            icon_border,
            label_height,
        );

        let colors = overlay.colors();
        let hdc = overlay.hdc_screen;
        let label = &self.label;
        let name = state.apps.get(state.index).map(|(_, _, n)| n.clone());
        overlay.render(x, y, width, height, dpi_scale, move |g| unsafe {
            GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
            draw_panel_border(g, colors, width, height, dpi_scale);

            // The icon row + cursor are centered vertically in the window; the name label
            // floats just below the cursor and does not affect this centering.
            let row_top = (height - item_size) / 2;

            // Rounded selection highlight behind the selected app, inset toward the icon so
            // it hugs the icon rather than filling the whole item cell.
            let sel_inset = icon_border - cursor_pad;
            let sel_size = item_size - 2 * sel_inset;
            let sel_left = (border_h + item_size * state.index as i32 + sel_inset) as f32;
            let sel_top = (row_top + sel_inset) as f32;
            let sel_radius = (sel_size as f32 / 4.0).min(8.0 * dpi_scale as f32);
            fill_round_rect(
                g,
                colors.selection,
                sel_left,
                sel_top,
                sel_size as f32,
                sel_size as f32,
                sel_radius,
            );

            // Icons. Build a correct premultiplied bitmap ourselves (GdipCreateBitmapFromHICON
            // ignores the AND-mask for non-alpha icons and fills them with opaque black).
            for (i, (icon, _, _)) in state.apps.iter().enumerate() {
                let Some((iw, ih, buf)) = hicon_to_pbgra(hdc, *icon) else {
                    continue;
                };
                let mut icon_ptr: *mut GpBitmap = std::ptr::null_mut();
                if GdipCreateBitmapFromScan0(
                    iw,
                    ih,
                    iw * 4,
                    PIXEL_FORMAT_32BPP_PARGB,
                    Some(buf.as_ptr()),
                    &mut icon_ptr,
                )
                .0 != 0
                    || icon_ptr.is_null()
                {
                    continue;
                }
                let ix = border_h + item_size * (i as i32) + icon_border;
                let iy = row_top + icon_border;
                GdipDrawImageRectI(g, icon_ptr as *mut GpImage, ix, iy, icon_size, icon_size);
                GdipDisposeImage(icon_ptr as *mut GpImage);
                // `buf` must outlive the draw above (GdipCreateBitmapFromScan0 does not copy).
                drop(buf);
            }

            // The selected app's name, centered beneath its own icon. The band is wider than
            // a single icon column so longer names fit before being ellipsized; it's centered
            // on the icon and clamped to stay within the pane.
            if let Some(name) = &name {
                let sel_center = border_h + item_size * state.index as i32 + item_size / 2;
                let avail = width - 2 * border_h;
                // Keep the band symmetric around the icon center so the centered text
                // stays under the selected icon. Near the edges we narrow it equally on
                // both sides rather than sliding it sideways (which would drift the
                // centered text onto the neighboring icon).
                let desired_half = ((item_size * 3).min(avail) / 2).max(item_size / 2);
                let half = desired_half
                    .min(sel_center - border_h)
                    .min(width - border_h - sel_center);
                // Center the (vertically-centered) text in the gap between the cursor's
                // bottom edge and the window's bottom edge.
                let cursor_bottom = row_top + sel_inset + sel_size;
                let rect = RectF {
                    X: (sel_center - half) as f32,
                    Y: cursor_bottom as f32,
                    Width: (half * 2) as f32,
                    Height: (height - cursor_bottom) as f32,
                };
                label.draw(g, name, TITLE_TEXT_COLOR, rect);
            }
        });
    }

    pub fn find_clicked_app_index(&self, overlay: &Overlay, state: &SwitchAppsState) -> Option<usize> {
        let cursor_pos = unsafe {
            let mut pos = POINT::default();
            let _ = GetCursorPos(&mut pos);
            pos
        };

        let dpi_scale = get_dpi_scale(overlay.hwnd());
        let icon_size_max = (ICON_SIZE_BASE as f64 * dpi_scale) as i32;
        let border_h = (APP_H_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let border_v = (APP_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let icon_border = (ICON_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let label_height = (APP_LABEL_HEIGHT_BASE as f64 * dpi_scale) as i32;

        let Coordinate {
            x,
            y,
            height,
            item_size,
            ..
        } = Coordinate::new(
            state.apps.len() as i32,
            icon_size_max,
            border_h,
            border_v,
            icon_border,
            label_height,
        );

        let xpos = cursor_pos.x - x;
        let ypos = cursor_pos.y - y;

        // Must match the vertically-centered icon row in `paint`.
        let cy = (height - item_size) / 2;
        for (i, _) in state.apps.iter().enumerate() {
            let cx = border_h + item_size * (i as i32);
            if xpos >= cx && xpos < cx + item_size && ypos >= cy && ypos < cy + item_size {
                return Some(i);
            }
        }
        None
    }
}

/// The Alt+Ctrl+Tab current-app window switcher: a vertical list of rows, each a small app
/// icon followed by the window title. Owns its GDI+ text objects (the shared `Overlay` owns
/// the GDI+ runtime lifetime, so this must be dropped before the `Overlay`).
pub struct WindowsPainter {
    text: TextRenderer,
}

impl WindowsPainter {
    /// Must be constructed AFTER `Overlay::new` (which initializes GDI+).
    pub fn new() -> Self {
        Self {
            text: TextRenderer::new(false, false),
        }
    }

    pub fn paint(&mut self, overlay: &mut Overlay, state: &SwitchAppWindowsState) {
        let dpi_scale = get_dpi_scale(overlay.hwnd());
        self.text
            .ensure_font(FONT_SIZE_BASE * dpi_scale as f32, dpi_scale);

        let border = (POPUP_BORDER_BASE as f64 * dpi_scale) as i32;
        let row_height = (ROW_HEIGHT_BASE as f64 * dpi_scale) as i32;
        let row_icon = (ROW_ICON_SIZE_BASE as f64 * dpi_scale) as i32;
        let pad = (ROW_TEXT_PAD_BASE as f64 * dpi_scale) as i32;
        let sel_inset = (ROW_SEL_INSET_BASE as f64 * dpi_scale).round() as i32;

        // Horizontal bands within a row: [border][pad][icon][pad][title][pad][border].
        // The title column is bounded so the text never spills past the selection highlight.
        let text_left = border + pad + row_icon + pad;
        let measured =
            unsafe { self.text.measure_max_width(state.items.iter().map(|(_, _, t)| t.as_str())) };
        let min_w = (POPUP_MIN_WIDTH_BASE as f64 * dpi_scale) as i32;
        let max_w = (POPUP_MAX_WIDTH_BASE as f64 * dpi_scale) as i32;
        let width = (text_left + measured + pad + border).clamp(min_w, max_w);

        let margin = Overlay::shadow_margin(dpi_scale);
        let VCoordinate {
            x,
            y,
            width,
            height,
            first_visible,
            visible_rows,
        } = VCoordinate::new(
            state.items.len(),
            state.index,
            width,
            border,
            row_height,
            margin,
        );

        let colors = overlay.colors();
        let hdc = overlay.hdc_screen;
        let text = &self.text;
        let items = &state.items;
        let sel_index = state.index;

        overlay.render(x, y, width, height, dpi_scale, move |g| unsafe {
            GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
            draw_panel_border(g, colors, width, height, dpi_scale);

            for row in 0..visible_rows {
                let idx = first_visible + row;
                let (icon, _, title) = &items[idx];
                let row_top = border + row_height * row as i32;

                // Selection highlight behind the current row (spans the inner pane width).
                if idx == sel_index {
                    let sel_radius = 6.0 * dpi_scale as f32;
                    fill_round_rect(
                        g,
                        colors.selection,
                        border as f32,
                        (row_top + sel_inset) as f32,
                        (width - 2 * border) as f32,
                        (row_height - 2 * sel_inset) as f32,
                        sel_radius,
                    );
                }

                // App icon (vertically centered in the row).
                if let Some((iw, ih, buf)) = hicon_to_pbgra(hdc, *icon) {
                    let mut icon_ptr: *mut GpBitmap = std::ptr::null_mut();
                    if GdipCreateBitmapFromScan0(
                        iw,
                        ih,
                        iw * 4,
                        PIXEL_FORMAT_32BPP_PARGB,
                        Some(buf.as_ptr()),
                        &mut icon_ptr,
                    )
                    .0 == 0
                        && !icon_ptr.is_null()
                    {
                        let ix = border + pad;
                        let iy = row_top + (row_height - row_icon) / 2;
                        GdipDrawImageRectI(g, icon_ptr as *mut GpImage, ix, iy, row_icon, row_icon);
                        GdipDisposeImage(icon_ptr as *mut GpImage);
                    }
                    drop(buf);
                }

                // Window title (left-aligned, vertically centered, ellipsized). The right edge
                // stops `pad` inside the selection so text never spills past the highlight.
                let rect = RectF {
                    X: text_left as f32,
                    Y: row_top as f32,
                    Width: (width - border - pad - text_left) as f32,
                    Height: row_height as f32,
                };
                text.draw(g, title, TITLE_TEXT_COLOR, rect);
            }
        });
    }

}

/// Owns the GDI+ font + string-format for drawing one line of text into the overlay's DIB.
/// The shared `Overlay` owns the GDI+ runtime, so a `TextRenderer` must be dropped before it
/// (guaranteed by App's field order: the painters are declared before the overlay).
struct TextRenderer {
    family: *mut GpFontFamily,
    format: *mut GpStringFormat,
    font: *mut GpFont,
    font_dpi: f64,
    bold: bool,
}

impl TextRenderer {
    /// `center` horizontally centers the text (otherwise left-aligns); always vertically
    /// centered, single line, ellipsized on overflow. `bold` selects the bold weight.
    /// Must be built AFTER `Overlay::new`.
    fn new(center: bool, bold: bool) -> Self {
        let mut family: *mut GpFontFamily = std::ptr::null_mut();
        let mut format: *mut GpStringFormat = std::ptr::null_mut();
        unsafe {
            // Failures leave null handles; text drawing is then skipped (icons still render).
            GdipCreateFontFamilyFromName(w!("Segoe UI"), std::ptr::null_mut(), &mut family);
            GdipCreateStringFormat(0, 0, &mut format);
            if !format.is_null() {
                let align = if center {
                    StringAlignmentCenter
                } else {
                    StringAlignmentNear
                };
                GdipSetStringFormatAlign(format, align);
                GdipSetStringFormatLineAlign(format, StringAlignmentCenter);
                GdipSetStringFormatTrimming(format, StringTrimmingEllipsisCharacter);
                GdipSetStringFormatFlags(format, StringFormatFlagsNoWrap.0);
            }
        }
        Self {
            family,
            format,
            font: std::ptr::null_mut(),
            font_dpi: 0.0,
            bold,
        }
    }

    /// (Re)creates the font when the DPI changes (`em_px` is in pixels).
    fn ensure_font(&mut self, em_px: f32, dpi_scale: f64) {
        if !self.font.is_null() && self.font_dpi == dpi_scale {
            return;
        }
        let style = if self.bold {
            FontStyleBold
        } else {
            FontStyleRegular
        };
        unsafe {
            if !self.font.is_null() {
                GdipDeleteFont(self.font);
                self.font = std::ptr::null_mut();
            }
            if !self.family.is_null() {
                let mut font: *mut GpFont = std::ptr::null_mut();
                GdipCreateFont(self.family, em_px, style.0, UnitPixel, &mut font);
                self.font = font;
            }
        }
        self.font_dpi = dpi_scale;
    }

    /// Draws `text` within `rect` (origin = pane top-left) in `color_argb`.
    unsafe fn draw(&self, g: *mut GpGraphics, text: &str, color_argb: u32, rect: RectF) {
        unsafe {
            if self.font.is_null() || self.format.is_null() {
                return;
            }
            let wide: Vec<u16> = text.encode_utf16().collect();
            let mut brush: *mut GpSolidFill = std::ptr::null_mut();
            GdipCreateSolidFill(color_argb, &mut brush);
            GdipDrawString(
                g,
                PCWSTR(wide.as_ptr()),
                wide.len() as i32,
                self.font,
                &rect,
                self.format,
                brush as *mut GpBrush,
            );
            GdipDeleteBrush(brush as *mut GpBrush);
            drop(wide);
        }
    }

    /// Widest of `texts` in px, measured with a throwaway GDI+ graphics. 0 if unavailable.
    unsafe fn measure_max_width<'a>(&self, texts: impl Iterator<Item = &'a str>) -> i32 {
        unsafe {
            if self.font.is_null() || self.format.is_null() {
                return 0;
            }
            let mut bmp: *mut GpBitmap = std::ptr::null_mut();
            if GdipCreateBitmapFromScan0(1, 1, 0, PIXEL_FORMAT_32BPP_PARGB, None, &mut bmp).0 != 0
                || bmp.is_null()
            {
                return 0;
            }
            let mut g: *mut GpGraphics = std::ptr::null_mut();
            GdipGetImageGraphicsContext(bmp as *mut GpImage, &mut g);

            let layout = RectF {
                X: 0.0,
                Y: 0.0,
                Width: 100_000.0,
                Height: 1_000.0,
            };
            let mut max_w = 0.0f32;
            for text in texts {
                let wide: Vec<u16> = text.encode_utf16().collect();
                let mut bbox = RectF::default();
                GdipMeasureString(
                    g,
                    PCWSTR(wide.as_ptr()),
                    wide.len() as i32,
                    self.font,
                    &layout,
                    self.format,
                    &mut bbox,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if bbox.Width > max_w {
                    max_w = bbox.Width;
                }
            }

            GdipDeleteGraphics(g);
            GdipDisposeImage(bmp as *mut GpImage);
            max_w.ceil() as i32
        }
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_null() {
                GdipDeleteFont(self.font);
            }
            if !self.format.is_null() {
                GdipDeleteStringFormat(self.format);
            }
            if !self.family.is_null() {
                GdipDeleteFontFamily(self.family);
            }
        }
    }
}

/// Converts a packed `0xAARRGGBB` value into a composition `Color`.
fn argb_color(argb: u32) -> Color {
    Color {
        A: (argb >> 24) as u8,
        R: (argb >> 16) as u8,
        G: (argb >> 8) as u8,
        B: argb as u8,
    }
}

/// Creates a hardware D3D11 device (with BGRA support for D2D), falling back to WARP.
fn create_d3d_device() -> Result<ID3D11Device> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device: Option<ID3D11Device> = None;
        let hr = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        if hr.is_ok() {
            if let Some(device) = device {
                return Ok(device);
            }
        }
    }
    Err(anyhow!("D3D11CreateDevice failed for hardware and WARP"))
}

fn query_effects_supported(hwnd: HWND) -> Result<bool> {
    let factory: ICompositionCapabilitiesInteropFactory =
        windows::core::factory::<CompositionCapabilities, ICompositionCapabilitiesInteropFactory>()?;
    let caps = unsafe { factory.GetForWindow(hwnd)? };
    Ok(caps.AreEffectsSupported()?)
}

/// Strokes the beveled rounded-rectangle outline of a pane (lit top-left, shadowed
/// bottom-right) — shared by both popups.
unsafe fn draw_panel_border(
    g: *mut GpGraphics,
    colors: SwitchAppsColors,
    width: i32,
    height: i32,
    dpi_scale: f64,
) {
    unsafe {
        // Rounded 1px stroke that follows the pane's rounded corners (a rectangular border
        // would be clipped at the corners by the composition rounded clip).
        let pen_width = (BORDER_WIDTH_BASE * dpi_scale as f32).max(1.0);
        let w = width as f32;
        let h = height as f32;
        // Centered stroke inset by half its width so its OUTER edge sits flush on the pane
        // edge (no sliver of tint showing outside the border).
        let inset = pen_width * 0.5;
        let corner = CORNER_RADIUS_BASE * dpi_scale as f32;
        let blur = BORDER_BLUR_BASE * dpi_scale as f32;
        stroke_round_rect(
            g,
            colors.border_light,
            colors.border_dark,
            inset,
            inset,
            w - 2.0 * inset,
            h - 2.0 * inset,
            (corner - inset).max(0.0),
            pen_width,
            blur,
        );
    }
}

/// Strokes a beveled rounded-rectangle outline: the top + left edges (and top-left corner)
/// in `light_color`, the bottom + right edges (and bottom-right corner) in `dark_color`,
/// splitting the two off-corners at 45°. Gives the panel a sense of depth (lit top-left).
#[allow(clippy::too_many_arguments)]
unsafe fn stroke_round_rect(
    g: *mut GpGraphics,
    light_color: u32,
    dark_color: u32,
    left: f32,
    top: f32,
    w: f32,
    h: f32,
    radius: f32,
    pen_width: f32,
    blur: f32,
) {
    unsafe {
        let d = radius * 2.0;
        let right = left + w;
        let bottom = top + h;

        // Light "L": bottom-left corner (from its 45° midpoint) up the left edge, around the
        // top-left corner, across the top edge, to the top-right corner's 45° midpoint.
        let mut light_path: *mut GpPath = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut light_path);
        GdipAddPathArc(light_path, left, bottom - d, d, d, 135.0, 45.0);
        GdipAddPathArc(light_path, left, top, d, d, 180.0, 90.0);
        GdipAddPathArc(light_path, right - d, top, d, d, 270.0, 45.0);
        stroke_path_soft(g, light_path, light_color, pen_width, blur);
        GdipDeletePath(light_path);

        // Dark "L": top-right 45° midpoint, down the right edge, around the bottom-right
        // corner, across the bottom edge, to the bottom-left corner's 45° midpoint.
        let mut dark_path: *mut GpPath = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut dark_path);
        GdipAddPathArc(dark_path, right - d, top, d, d, 315.0, 45.0);
        GdipAddPathArc(dark_path, right - d, bottom - d, d, d, 0.0, 90.0);
        GdipAddPathArc(dark_path, left, bottom - d, d, d, 90.0, 45.0);
        stroke_path_soft(g, dark_path, dark_color, pen_width, blur);
        GdipDeletePath(dark_path);
    }
}

/// Strokes a path several times — a wide faint pass plus narrower stronger ones — to fake
/// a soft, blurred edge instead of a hard 1px line.
unsafe fn stroke_path_soft(
    g: *mut GpGraphics,
    path: *mut GpPath,
    color_argb: u32,
    core_width: f32,
    blur: f32,
) {
    unsafe {
        let rgb = color_argb & 0x00ff_ffff;
        let base_a = ((color_argb >> 24) & 0xff) as f32;
        // (extra width, alpha factor), widest+faintest first so the core lands on top.
        for (extra, factor) in [(blur * 2.0, 0.22f32), (blur, 0.45), (0.0, 1.0)] {
            let a = (base_a * factor).round() as u32;
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1((a << 24) | rgb, core_width + extra, Unit(2), &mut pen);
            GdipDrawPath(g, pen, path);
            GdipDeletePen(pen);
        }
    }
}

/// Fills an antialiased rounded rectangle with a solid ARGB color (GDI+).
unsafe fn fill_round_rect(
    g: *mut GpGraphics,
    color_argb: u32,
    left: f32,
    top: f32,
    w: f32,
    h: f32,
    radius: f32,
) {
    unsafe {
        let d = radius * 2.0;
        let right = left + w;
        let bottom = top + h;
        let mut brush: *mut GpSolidFill = std::ptr::null_mut();
        GdipCreateSolidFill(color_argb, &mut brush);
        let mut path: *mut GpPath = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut path);
        GdipAddPathArc(path, left, top, d, d, 180.0, 90.0);
        GdipAddPathArc(path, right - d, top, d, d, 270.0, 90.0);
        GdipAddPathArc(path, right - d, bottom - d, d, d, 0.0, 90.0);
        GdipAddPathArc(path, left, bottom - d, d, d, 90.0, 90.0);
        GdipClosePathFigure(path);
        GdipFillPath(g, brush as *mut GpBrush, path);
        GdipDeletePath(path);
        GdipDeleteBrush(brush as *mut GpBrush);
    }
}

/// Extracts an HICON into a premultiplied BGRA buffer (`width, height, pixels`), using
/// the icon's alpha channel when present and otherwise deriving alpha from its AND-mask.
/// This is the correct path that `GdipCreateBitmapFromHICON` gets wrong for mask icons.
unsafe fn hicon_to_pbgra(hdc: HDC, hicon: HICON) -> Option<(i32, i32, Vec<u8>)> {
    unsafe {
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            return None;
        }
        let color = info.hbmColor;
        let mask = info.hbmMask;

        let cleanup = || {
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
        };

        let mut bmp = BITMAP::default();
        if GetObjectW(
            color.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut _ as *mut c_void),
        ) == 0
        {
            cleanup();
            return None;
        }
        let (w, h) = (bmp.bmWidth, bmp.bmHeight);
        if w <= 0 || h <= 0 {
            cleanup();
            return None;
        }

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let count = (w * h) as usize;
        let mut buf = vec![0u8; count * 4];
        if GetDIBits(
            hdc,
            color,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        ) == 0
        {
            cleanup();
            return None;
        }

        // No alpha channel? Derive it from the AND-mask (mask black = opaque).
        if !buf.chunks_exact(4).any(|p| p[3] != 0) {
            let mut mask_buf = vec![0u8; count * 4];
            if GetDIBits(
                hdc,
                mask,
                0,
                h as u32,
                Some(mask_buf.as_mut_ptr() as *mut c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            ) != 0
            {
                for (p, m) in buf.chunks_exact_mut(4).zip(mask_buf.chunks_exact(4)) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
        }

        // Premultiply for the PARGB target surface.
        for p in buf.chunks_exact_mut(4) {
            let a = p[3] as u32;
            p[0] = (p[0] as u32 * a / 255) as u8;
            p[1] = (p[1] as u32 * a / 255) as u8;
            p[2] = (p[2] as u32 * a / 255) as u8;
        }

        cleanup();
        Some((w, h, buf))
    }
}

fn get_dpi_scale(hwnd: HWND) -> f64 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            1.0
        } else {
            dpi as f64 / 96.0
        }
    }
}

/// Horizontal layout for the Alt+Tab app row.
struct Coordinate {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    icon_size: i32,
    item_size: i32,
}

impl Coordinate {
    fn new(
        num_apps: i32,
        icon_size_max: i32,
        border_h: i32,
        border_v: i32,
        icon_border: i32,
        label_height: i32,
    ) -> Self {
        let monitor_rect = get_moinitor_rect();
        let monitor_width = monitor_rect.right - monitor_rect.left;
        let monitor_height = monitor_rect.bottom - monitor_rect.top;

        let icon_size =
            ((monitor_width - 2 * border_h) / num_apps - icon_border * 2).min(icon_size_max);

        let item_size = icon_size + icon_border * 2;
        let width = item_size * num_apps + border_h * 2;
        // Top margin + icon row + name band + bottom margin (vertical margins top and bottom).
        let height = border_v + item_size + label_height + border_v;
        let x = monitor_rect.left + (monitor_width - width) / 2;
        let y = monitor_rect.top + (monitor_height - height) / 2;

        Self {
            x,
            y,
            width,
            height,
            icon_size,
            item_size,
        }
    }
}

/// Vertical layout for the Alt+Ctrl+Tab window list. When more rows exist than fit on the
/// monitor, only a window of `visible_rows` (scrolled to keep `index` in view) is shown.
struct VCoordinate {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    first_visible: usize,
    visible_rows: usize,
}

impl VCoordinate {
    fn new(
        num_rows: usize,
        index: usize,
        width: i32,
        border_size: i32,
        row_height: i32,
        margin: i32,
    ) -> Self {
        let monitor_rect = get_moinitor_rect();
        let monitor_width = monitor_rect.right - monitor_rect.left;
        let monitor_height = monitor_rect.bottom - monitor_rect.top;

        // How many rows fit, leaving room for the border and the shadow margin.
        let avail = (monitor_height - 2 * margin - 2 * border_size).max(row_height);
        let max_rows = (avail / row_height).max(1) as usize;
        let visible_rows = num_rows.min(max_rows).max(1);

        let first_visible = if num_rows <= visible_rows {
            0
        } else {
            let half = visible_rows / 2;
            index.saturating_sub(half).min(num_rows - visible_rows)
        };

        let height = row_height * visible_rows as i32 + border_size * 2;
        let x = monitor_rect.left + (monitor_width - width) / 2;
        let y = monitor_rect.top + (monitor_height - height) / 2;

        Self {
            x,
            y,
            width,
            height,
            first_visible,
            visible_rows,
        }
    }
}
