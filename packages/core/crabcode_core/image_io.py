"""Decode local raster images into image formats accepted by vision models.

Call from a filesystem worker: image decoding can be slow or block on storage.
PSD reads use the saved merged image, not an individual layer.
"""

from __future__ import annotations

from dataclasses import dataclass
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageOps

from crabcode_core.types import tool as tool_types


_IMAGE_EXTENSIONS = {
    ".png", ".jpg", ".jpeg", ".jpe", ".webp", ".gif", ".bmp", ".dib",
    ".tif", ".tiff", ".ico", ".psd", ".psb", ".ppm", ".pgm", ".pbm",
    ".tga", ".jp2", ".j2k", ".avif", ".heic", ".heif",
}
# Restrict decoding to raster plugins; do not invoke external renderers (EPS/PDF).
_FORMATS = ("PNG", "JPEG", "WEBP", "GIF", "BMP", "TIFF", "ICO", "PSD",
            "PPM", "TGA", "JPEG2000", "AVIF")
_NATIVE_MIME = {"PNG": "image/png", "JPEG": "image/jpeg", "WEBP": "image/webp"}
MAX_PREVIEW_DIMENSION = 4096
MAX_DECODE_PIXELS = 40_000_000


def is_image_file(path: Path) -> bool:
    """Recognize common raster files, including files without an image suffix.

SVG and other text-based graphics remain readable as source with Read.
"""
    if path.suffix.lower() in _IMAGE_EXTENSIONS:
        return True
    with path.open("rb") as stream:
        header = stream.read(16)
    return (
        header.startswith((b"\x89PNG\r\n\x1a\n", b"\xff\xd8\xff", b"GIF87a", b"GIF89a",
                           b"II*\0", b"MM\0*", b"II+\0", b"MM\0+", b"8BPS",
                           b"\0\0\1\0"))
        or (header.startswith(b"RIFF") and header[8:12] == b"WEBP")
        or (header.startswith(b"BM") and header[6:10] == b"\0\0\0\0")
        or (header[4:8] == b"ftyp" and header[8:12] in {b"avif", b"avis", b"heic", b"mif1"})
    )


@dataclass
class ImagePreview:
    data: bytes
    media_type: str
    width: int
    height: int
    source_format: str
    original_size: tuple[int, int]
    first_frame: bool

    def summary(self) -> str:
        details = f"{self.width}x{self.height}, {self.media_type}, {len(self.data)} bytes"
        if self.source_format == "PSD":
            details += "; saved PSD composite preview (layers are not included)"
        elif self.first_frame:
            details += "; first frame/page only"
        if self.original_size != (self.width, self.height):
            details += f"; original {self.original_size[0]}x{self.original_size[1]}"
        return details


def load_image_preview(path: Path) -> ImagePreview:
    """Validate pixels and normalize non-native, animated, or oversized images."""
    try:
        with Image.open(path, formats=_FORMATS) as source:
            source_format = source.format or "unknown"
            original_size = source.size
            if source.width * source.height > MAX_DECODE_PIXELS:
                raise ValueError(f"image exceeds the {MAX_DECODE_PIXELS:,}-pixel decode limit")
            # PSD exposes layers as frames; frame 0 is its saved merged image.
            first_frame = source_format != "PSD" and getattr(source, "is_animated", False)
            orientation = source.getexif().get(274, 1)
            source.load()
            native_mime = _NATIVE_MIME.get(source_format)
            if (native_mime and not first_frame and orientation == 1
                    and source.mode in {"RGB", "RGBA", "L", "LA", "P"}
                    and max(source.size) <= MAX_PREVIEW_DIMENSION
                    and path.stat().st_size <= tool_types.MAX_INLINE_IMAGE_BYTES):
                return ImagePreview(path.read_bytes(), native_mime, *source.size,
                                    source_format, original_size, False)

            preview = ImageOps.exif_transpose(source)
            preview = preview.convert("RGBA" if "A" in preview.getbands()
                                      or "transparency" in preview.info else "RGB")
            preview.thumbnail((MAX_PREVIEW_DIMENSION, MAX_PREVIEW_DIMENSION), Image.Resampling.LANCZOS)
            # Do not copy arbitrary source metadata into a model attachment.
            preview.info.clear()
            while True:
                output = BytesIO()
                preview.save(output, format="PNG")
                data = output.getvalue()
                if len(data) <= tool_types.MAX_INLINE_IMAGE_BYTES:
                    return ImagePreview(data, "image/png", *preview.size, source_format,
                                        original_size, bool(first_frame))
                if max(preview.size) <= 1:
                    raise ValueError("image preview exceeds the attachment byte limit")
                preview = preview.resize((max(1, preview.width * 3 // 4),
                                          max(1, preview.height * 3 // 4)), Image.Resampling.LANCZOS)
    except (OSError, ValueError, SyntaxError, Image.DecompressionBombError) as exc:
        raise ValueError(
            f"Cannot decode image {path}: {exc}. Export a flattened PNG or JPEG and read that file."
        ) from exc
