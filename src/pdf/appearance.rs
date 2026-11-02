use image::ImageEncoder;
use mupdf::pdf::{PdfAnnotation, PdfDocument, PdfObject};
use mupdf::{Buffer, Colorspace, Image, Pixmap, Rect};

use super::annot_read::MarkerKind;
use super::engine::Error;

pub(crate) fn install_math_appearance(
    dest: &mut PdfDocument,
    annot: &mut PdfAnnotation,
    pdf_bytes: &[u8],
) -> Result<(), mupdf::Error> {
    let src = PdfDocument::from_bytes(pdf_bytes)?;
    let src_page = src.load_pdf_page(0)?;
    let media = src_page.media_box()?;
    let Some(contents) = src_page.contents()? else {
        return Err(mupdf::Error::InvalidArgument(
            "equation PDF has no content".into(),
        ));
    };
    let resources = dest.graft_object(&src_page.resources()?)?;
    let form = form_xobject(dest, &contents, resources, media)?;
    let mut appearance = dest.new_dict()?;
    appearance.dict_put("N", form)?;
    annot.object().dict_put("AP", appearance)?;
    Ok(())
}

pub(crate) fn write_image_marker(
    doc: &mut PdfDocument,
    annot: &PdfAnnotation,
    id: u64,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(), Error> {
    let png = encode_png(rgba, width, height)?;
    let id_i32 = i32::try_from(id).map_err(|_| Error::from("annotation id does not fit in i32"))?;
    let mut marker = doc.new_dict()?;
    marker.dict_put("Kind", doc.new_name(MarkerKind::Image.as_str())?)?;
    marker.dict_put("Id", doc.new_int(id_i32)?)?;
    marker.dict_put(
        "PixelHash",
        doc.new_string(&format!("{:016x}", pixel_hash(rgba)))?,
    )?;
    let buffer = Buffer::from_bytes(&png)?;
    let stream = doc.add_stream(&buffer, None, true)?;
    marker.dict_put("Png", stream)?;
    let mut object = annot.object();
    object.dict_put("Marker", marker)?;
    object.dict_put("NM", doc.new_string(&format!("marker-{id}"))?)?;
    Ok(())
}

pub(crate) fn pixel_hash(rgba: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in rgba {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(crate) fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(Error::Message(format!(
            "rgba length {} != {}x{}x4",
            rgba.len(),
            width,
            height
        )));
    }
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    encoder
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|_| Error::Encode)?;
    Ok(out)
}

pub(crate) fn install_image_appearance(
    dest: &mut PdfDocument,
    annot: &mut PdfAnnotation,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(), mupdf::Error> {
    if width == 0 || height == 0 {
        return Err(mupdf::Error::InvalidArgument("empty image".into()));
    }
    let cs = Colorspace::device_rgb();
    let mut pixmap = Pixmap::new_with_w_h(&cs, width as i32, height as i32, true)?;
    let n = pixmap.n() as usize;
    let stride = pixmap.stride() as usize;
    let row_bytes = width as usize * 4;
    let samples = pixmap.samples_mut();
    for y in 0..height as usize {
        let src = &rgba[y * row_bytes..(y + 1) * row_bytes];
        let dst = &mut samples[y * stride..y * stride + width as usize * n];
        if n == 4 {
            dst.copy_from_slice(src);
        } else if n == 3 {
            for x in 0..width as usize {
                dst[x * 3] = src[x * 4];
                dst[x * 3 + 1] = src[x * 4 + 1];
                dst[x * 3 + 2] = src[x * 4 + 2];
            }
        } else {
            return Err(mupdf::Error::InvalidArgument(format!(
                "unexpected pixmap components {n}"
            )));
        }
    }
    let image = Image::from_pixmap(&pixmap)?;
    let image_obj = dest.add_image(&image)?;
    let w = width as f32;
    let h = height as f32;
    let content = format!("q\n{w} 0 0 {h} 0 0 cm\n/Im0 Do\nQ\n");
    let buffer = Buffer::from_bytes(content.as_bytes())?;
    let mut xobjects = dest.new_dict()?;
    xobjects.dict_put("Im0", image_obj)?;
    let mut resources = dest.new_dict()?;
    resources.dict_put("XObject", xobjects)?;
    let dict = dest.new_dict()?;
    let mut form = dest.add_stream(&buffer, Some(&dict), true)?;
    let mut bbox = dest.new_array()?;
    Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w,
        y1: h,
    }
    .encode_into(&mut bbox)?;
    form.dict_put("Type", dest.new_name("XObject")?)?;
    form.dict_put("Subtype", dest.new_name("Form")?)?;
    form.dict_put("BBox", bbox)?;
    form.dict_put("Resources", resources)?;
    let mut appearance = dest.new_dict()?;
    appearance.dict_put("N", form)?;
    annot.object().dict_put("AP", appearance)?;
    Ok(())
}

pub(crate) fn form_xobject(
    dest: &mut PdfDocument,
    contents: &PdfObject,
    resources: PdfObject,
    media: Rect,
) -> Result<PdfObject, mupdf::Error> {
    let resolved = contents.resolve()?.unwrap_or(contents.try_clone()?);
    let mut form = if resolved.is_array()? {
        let mut bytes = Vec::new();
        for index in 0..resolved.len()? {
            let Some(item) = resolved.get_array(index as i32)? else {
                continue;
            };
            let item = item.resolve()?.unwrap_or(item);
            if item.is_stream()? {
                bytes.extend(item.read_stream()?);
                bytes.push(b'\n');
            }
        }
        let dict = dest.new_dict()?;
        let buffer = mupdf::Buffer::from_bytes(&bytes)?;
        dest.add_stream(&buffer, Some(&dict), true)?
    } else {
        dest.graft_object(contents)?
    };
    let mut bbox = dest.new_array()?;
    media.encode_into(&mut bbox)?;
    form.dict_put("Type", dest.new_name("XObject")?)?;
    form.dict_put("Subtype", dest.new_name("Form")?)?;
    form.dict_put("BBox", bbox)?;
    form.dict_put("Resources", resources)?;
    Ok(form)
}
