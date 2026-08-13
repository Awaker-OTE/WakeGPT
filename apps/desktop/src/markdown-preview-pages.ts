export const MARKDOWN_LIVE_PREVIEW_MAX_BYTES = 512 * 1024;
export const MARKDOWN_PREVIEW_PAGE_MAX_BYTES = 384 * 1024;
export const MARKDOWN_DOCUMENT_MAX_BYTES = 16 * 1024 * 1024;

const utf8Encoder = new TextEncoder();
const markdownUtf8Bytes = (value: string): number => utf8Encoder.encode(value).byteLength;

const codeFence = (line: string): { character: "`" | "~"; length: number } | null => {
  const match = /^ {0,3}(`{3,}|~{3,})/u.exec(line);
  if (!match) return null;
  return { character: match[1][0] as "`" | "~", length: match[1].length };
};

const closesFence = (
  line: string,
  fence: { character: "`" | "~"; length: number },
): boolean => new RegExp(`^ {0,3}${fence.character}{${fence.length},}[ \t]*$`, "u").test(line);

export interface MarkdownPreviewPage {
  start: number;
  end: number;
  byteLength: number;
  markdown: string;
  oversizedBlock: boolean;
}

export interface MarkdownEditingPage {
  markdown: string;
  plainTextPreview: boolean;
}

const splitUtf8 = (value: string, maxBytes: number): string[] => {
  const chunks: string[] = [];
  let start = 0;
  let offset = 0;
  let bytes = 0;
  while (offset < value.length) {
    const first = value.charCodeAt(offset);
    const surrogatePair = first >= 0xd800
      && first <= 0xdbff
      && value.charCodeAt(offset + 1) >= 0xdc00
      && value.charCodeAt(offset + 1) <= 0xdfff;
    const units = surrogatePair ? 2 : 1;
    const nextBytes = surrogatePair
      ? 4
      : first <= 0x7f
        ? 1
        : first <= 0x7ff
          ? 2
          : 3;
    if (bytes && bytes + nextBytes > maxBytes) {
      chunks.push(value.slice(start, offset));
      start = offset;
      bytes = 0;
      continue;
    }
    bytes += nextBytes;
    offset += units;
  }
  if (start < value.length || !chunks.length) chunks.push(value.slice(start));
  return chunks;
};

export function paginateMarkdownPreview(
  markdown: string,
  pageMaxBytes = MARKDOWN_PREVIEW_PAGE_MAX_BYTES,
): MarkdownPreviewPage[] {
  if (!Number.isSafeInteger(pageMaxBytes) || pageMaxBytes < 1024) {
    throw new Error("Markdown preview page size is invalid");
  }
  if (!markdown) {
    return [{ start: 0, end: 0, byteLength: 0, markdown: "", oversizedBlock: false }];
  }

  const pages: MarkdownPreviewPage[] = [];
  let pageStart = 0;
  let pageEnd = pageStart;
  let pageBytes = 0;
  const appendPage = (oversizedBlock: boolean) => {
    pages.push({
      start: pageStart,
      end: pageEnd,
      byteLength: pageBytes,
      markdown: markdown.slice(pageStart, pageEnd),
      oversizedBlock,
    });
  };
  const appendBlock = (block: { start: number; end: number; byteLength: number }) => {
    if (pageBytes && pageBytes + block.byteLength > pageMaxBytes) {
      appendPage(false);
      pageStart = block.start;
      pageEnd = block.start;
      pageBytes = 0;
    }
    if (!pageBytes && block.byteLength > pageMaxBytes) {
      let chunkStart = block.start;
      for (const chunk of splitUtf8(markdown.slice(block.start, block.end), pageMaxBytes)) {
        const chunkEnd = chunkStart + chunk.length;
        pages.push({
          start: chunkStart,
          end: chunkEnd,
          byteLength: markdownUtf8Bytes(chunk),
          markdown: chunk,
          oversizedBlock: true,
        });
        chunkStart = chunkEnd;
      }
      pageStart = block.end;
      pageEnd = block.end;
      pageBytes = 0;
      return;
    }
    if (!pageBytes) pageStart = block.start;
    pageEnd = block.end;
    pageBytes += block.byteLength;
  };

  let blockStart = 0;
  let offset = 0;
  let fence: { character: "`" | "~"; length: number } | null = null;
  while (offset < markdown.length) {
    const lineStart = offset;
    while (offset < markdown.length && markdown[offset] !== "\n" && markdown[offset] !== "\r") {
      offset += 1;
    }
    const content = markdown.slice(lineStart, offset);
    if (markdown[offset] === "\r" && markdown[offset + 1] === "\n") offset += 2;
    else if (offset < markdown.length) offset += 1;
    const marker = codeFence(content);
    if (!fence && marker) fence = marker;
    else if (fence && closesFence(content, fence)) fence = null;
    if (!fence && content.trim() === "") {
      appendBlock({
        start: blockStart,
        end: offset,
        byteLength: markdownUtf8Bytes(markdown.slice(blockStart, offset)),
      });
      blockStart = offset;
    }
  }
  if (blockStart < markdown.length) {
    appendBlock({
      start: blockStart,
      end: markdown.length,
      byteLength: markdownUtf8Bytes(markdown.slice(blockStart)),
    });
  }
  if (pageBytes || !pages.length) appendPage(false);
  return pages;
}

export const markdownPreviewRequiresPaging = (markdown: string): boolean => (
  markdown.length > MARKDOWN_LIVE_PREVIEW_MAX_BYTES
  || markdownUtf8Bytes(markdown) > MARKDOWN_LIVE_PREVIEW_MAX_BYTES
);

export function paginateMarkdownEditing(markdown: string): MarkdownEditingPage[] {
  const editingPages: MarkdownEditingPage[] = [];
  for (const page of paginateMarkdownPreview(markdown)) {
    if (page.oversizedBlock) {
      for (const chunk of splitUtf8(page.markdown, MARKDOWN_PREVIEW_PAGE_MAX_BYTES)) {
        editingPages.push({ markdown: chunk, plainTextPreview: true });
      }
    } else {
      editingPages.push({ markdown: page.markdown, plainTextPreview: false });
    }
  }
  return editingPages;
}
