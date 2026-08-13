export const MAX_IMAGES_PER_RECORD = 10;
export const MAX_IMAGE_BYTES = 20 * 1024 * 1024;
export const MAX_RECORD_IMAGE_BYTES = 100 * 1024 * 1024;

interface ImageFileDescriptor {
  name: string;
  size: number;
  type: string;
}

interface ExistingImageDescriptor {
  byteSize: number;
}

const mediaExtensions: Record<string, string[]> = {
  "image/gif": ["gif"],
  "image/jpeg": ["jpg", "jpeg"],
  "image/png": ["png"],
  "image/webp": ["webp"],
};

function fileExtension(name: string): string {
  const separator = name.lastIndexOf(".");
  return separator >= 0 ? name.slice(separator + 1).toLocaleLowerCase() : "";
}

export function isSupportedImageFile(file: ImageFileDescriptor): boolean {
  const mediaType = file.type.trim().toLocaleLowerCase();
  if (mediaType) return Object.prototype.hasOwnProperty.call(mediaExtensions, mediaType);
  const extension = fileExtension(file.name);
  return Object.values(mediaExtensions).some((extensions) => extensions.includes(extension));
}

export function validateImageFiles(
  files: readonly ImageFileDescriptor[],
  existing: readonly ExistingImageDescriptor[],
): string | null {
  if (!files.length || files.some((file) => !isSupportedImageFile(file))) {
    return "仅支持 PNG、JPEG、WebP 和 GIF 图片。";
  }
  if (files.some((file) => !Number.isSafeInteger(file.size) || file.size <= 0)) {
    return "图片文件为空或不可读取。";
  }
  if (files.some((file) => file.size > MAX_IMAGE_BYTES)) {
    return "单张图片不能超过 20 MiB。";
  }
  if (existing.length + files.length > MAX_IMAGES_PER_RECORD) {
    return "每条记录最多 10 张图片。";
  }
  const total = [...existing, ...files].reduce(
    (sum, image) => sum + ("size" in image ? image.size : image.byteSize),
    0,
  );
  if (!Number.isSafeInteger(total) || total > MAX_RECORD_IMAGE_BYTES) {
    return "单条记录的图片合计不能超过 100 MiB。";
  }
  return null;
}

function truncateUtf8(value: string, maxBytes: number): string {
  const encoder = new TextEncoder();
  let size = 0;
  let result = "";
  for (const character of value) {
    const characterBytes = encoder.encode(character).byteLength;
    if (size + characterBytes > maxBytes) break;
    result += character;
    size += characterBytes;
  }
  return result;
}

export function safeImageDisplayName(name: string, mediaType: string): string {
  const extensions = mediaExtensions[mediaType] ?? ["img"];
  const normalized = Array.from(name.trim())
    .map((character) => {
      const code = character.codePointAt(0) ?? 0;
      return character === "/" || character === "\\" || code < 32 || code === 127
        ? "_"
        : character;
    })
    .join("");
  const hasExpectedExtension = extensions.includes(fileExtension(normalized));
  const extension = extensions[0];
  if (hasExpectedExtension) {
    return truncateUtf8(normalized, 480) || `图片.${extension}`;
  }
  const base = truncateUtf8(normalized || "图片", 470).replace(/[. ]+$/u, "") || "图片";
  return `${base}.${extension}`;
}
