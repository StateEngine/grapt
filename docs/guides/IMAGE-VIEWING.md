# Viewing local images

Ask the model to view a workspace image, or call `view_image` with its path:

```json
{ "path": "screenshots/error.png", "detail": "high" }
```

The tool sends actual image content to models that advertise image input. It uses the
same read permission as `read`. PNG, JPEG, GIF and WebP are supported; animated GIFs
use their first frame. Images remain available after reopening a session, even if
the original file has moved or been deleted. Conversation history shows a short
description instead of encoded pixels.

`high` is the default: it corrects orientation, never enlarges the image, and resizes
to at most 2048 pixels on the longest side. PNG preserves transparency; opaque images
may use JPEG to fit the payload limit. `original` preserves resolution up to 4096
pixels and 700 KiB. Other than GIF conversion, it preserves the source bytes. Its
provider detail falls back to `high` unless the selected model advertises original
detail support. Use `high` if an original image is too large.

Source files must be at most 16 MiB, with each dimension at most 16,384 pixels and
at most 32 megapixels. Decode allocation is capped at 192 MiB; prepared image bytes
are capped at 700 KiB. Unsupported, corrupt or oversized files return a tool error.
Switching to a text-only model omits image content with an explicit notice; switching
back restores the saved image.
