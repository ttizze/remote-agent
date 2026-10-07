@file:Suppress("TooGenericExceptionCaught") // Document providers surface heterogeneous I/O errors.

package dev.remoteagent.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.OpenableColumns
import dev.remoteagent.core.AttachmentCandidate
import dev.remoteagent.core.AttachmentFileKind
import dev.remoteagent.core.DraftAttachment
import dev.remoteagent.core.LocalFile
import dev.remoteagent.core.admitAttachments
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import kotlin.math.roundToInt

private const val COPY_BUFFER_BYTES = 64 * 1024
private const val MAX_FILE_BYTES = 50L * 1024 * 1024
private const val MAX_IMAGE_BYTES = 10L * 1024 * 1024

/** Longest edge kept when a photo is re-encoded, as every client hands providers. */
private const val PHOTO_MAX_EDGE = 2048
private const val PHOTO_JPEG_QUALITY = 85
private val PROVIDER_IMAGE_TYPES = setOf("image/png", "image/jpeg", "image/gif", "image/webp")

/** Files ready for `Intent.AttachFiles`, and the last refusal of the batch. */
internal data class PreparedAttachments(val files: List<LocalFile>, val error: String?)

private data class CopiedFile(val file: File, val name: String, val mime: String)

private fun attachmentDirectory(context: Context) = File(context.filesDir, "draft-attachments").apply { mkdirs() }

private fun copyStream(input: InputStream, output: OutputStream) {
    val buffer = ByteArray(COPY_BUFFER_BYTES)
    var total = 0L
    while (true) {
        val count = input.read(buffer)
        if (count < 0) return
        total += count
        require(total <= MAX_FILE_BYTES) { "File exceeds 50 MB" }
        output.write(buffer, 0, count)
    }
}

private fun copyPicked(context: Context, uri: Uri): CopiedFile {
    val name =
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        } ?: ""
    val mime = context.contentResolver.getType(uri) ?: ""
    val file = File(attachmentDirectory(context), UUID.randomUUID().toString())
    try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            file.outputStream().use { output -> copyStream(input, output) }
        } ?: error("Unable to read '$name'")
    } catch (failure: Exception) {
        file.delete()
        throw failure
    }
    return CopiedFile(file, name, mime)
}

/** Decodes with EXIF orientation applied, downscales to the edge limit and writes a JPEG. */
private fun renderJpeg(context: Context, source: File): File {
    val image =
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(source)) { decoder, info, _ ->
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            val longest = maxOf(info.size.width, info.size.height)
            if (longest > PHOTO_MAX_EDGE) {
                val scale = PHOTO_MAX_EDGE.toFloat() / longest
                decoder.setTargetSize(
                    (info.size.width * scale).roundToInt().coerceAtLeast(1),
                    (info.size.height * scale).roundToInt().coerceAtLeast(1),
                )
            }
        }
    val output = File(attachmentDirectory(context), "${UUID.randomUUID()}.jpg")
    try {
        output.outputStream().use {
            require(image.compress(Bitmap.CompressFormat.JPEG, PHOTO_JPEG_QUALITY, it)) { "Unable to encode photo" }
        }
    } catch (failure: Exception) {
        output.delete()
        throw failure
    } finally {
        image.recycle()
    }
    return output
}

private fun jpegName(name: String) =
    if (Regex("\\.jpe?g$", RegexOption.IGNORE_CASE).containsMatchIn(name)) name
    else name.replace(Regex("\\.[^.]+$"), "") + ".jpg"

/**
 * Copies picked files, lets core admit them, and re-encodes images providers cannot read as sent or that exceed the
 * image limit.
 */
internal fun prepareAttachments(
    context: Context,
    uris: List<Uri>,
    existing: List<DraftAttachment>,
): PreparedAttachments {
    var error: String? = null
    val copied = uris.mapNotNull { uri ->
        try {
            copyPicked(context, uri)
        } catch (failure: Exception) {
            error = failure.message
            null
        }
    }
    val admission =
        admitAttachments(existing, copied.map { AttachmentCandidate(it.name, it.mime, it.file.length().toULong()) })
    val accepted = admission.accepted.associateBy { it.index.toInt() }
    val files = copied.mapIndexedNotNull { index, file ->
        val admitted = accepted[index]
        if (admitted == null) {
            file.file.delete()
            return@mapIndexedNotNull null
        }
        val render =
            admitted.kind == AttachmentFileKind.IMAGE &&
                (admitted.needsCompression || admitted.mimeType.lowercase() !in PROVIDER_IMAGE_TYPES)
        if (!render) return@mapIndexedNotNull LocalFile(file.file.path, admitted.name, admitted.mimeType)
        try {
            val jpeg = renderJpeg(context, file.file)
            if (jpeg.length() > MAX_IMAGE_BYTES) {
                jpeg.delete()
                error = "'${admitted.name}' is too large to attach, even after compression."
                null
            } else LocalFile(jpeg.path, jpegName(admitted.name), "image/jpeg")
        } catch (_: Exception) {
            error = "'${admitted.name}' could not be read as an image."
            null
        } finally {
            file.file.delete()
        }
    }
    return PreparedAttachments(files, admission.error ?: error)
}
