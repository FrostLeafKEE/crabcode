import { describe, expect, it } from "vitest";
import { randomUuid } from "./uuid";

describe("randomUuid", () => {
  it("uses crypto.randomUUID when the webview allows it", () => {
    const expected = "00000000-0000-4000-8000-000000000001";
    expect(randomUuid({ randomUUID: () => expected })).toBe(expected);
  });

  it("falls back to getRandomValues when randomUUID is blocked", () => {
    const uuid = randomUuid({
      randomUUID: () => {
        throw new DOMException("The operation is insecure", "SecurityError");
      },
      getRandomValues: (bytes) => {
        bytes.fill(0xab);
        return bytes;
      },
    });

    expect(uuid).toBe("abababab-abab-4bab-abab-abababababab");
  });

  it("still returns an RFC 4122 version 4 UUID without Web Crypto", () => {
    expect(randomUuid(null)).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
  });
});
