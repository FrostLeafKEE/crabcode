interface UuidCrypto {
  randomUUID?: () => string;
  getRandomValues?: (array: Uint8Array) => Uint8Array;
}

function browserCrypto(): UuidCrypto | null {
  return typeof globalThis.crypto === "undefined"
    ? null
    : globalThis.crypto as UuidCrypto;
}

export function randomUuid(source: UuidCrypto | null = browserCrypto()): string {
  if (source?.randomUUID) {
    try {
      return source.randomUUID();
    } catch {
      // WKWebView can expose randomUUID while rejecting it for tauri:// pages.
    }
  }

  const bytes = new Uint8Array(16);
  if (source?.getRandomValues) {
    try {
      source.getRandomValues(bytes);
    } catch {
      for (let index = 0; index < bytes.length; index += 1) {
        bytes[index] = Math.floor(Math.random() * 256);
      }
    }
  } else {
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256);
    }
  }

  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, "0"));
  return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
}
