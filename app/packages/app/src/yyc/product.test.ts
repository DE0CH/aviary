import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_PRODUCT_NAME,
  initialProductName,
  onProductNameChange,
  productName,
  setProductName,
  withProductName,
} from "./product";

describe("the product's name", () => {
  afterEach(() => setProductName(DEFAULT_PRODUCT_NAME));

  it("is YYC AI until the server names another", () => {
    expect(productName()).toBe("YYC AI");
    expect(withProductName("Message YYC AI")).toBe("Message YYC AI");
    let changes = 0;
    const off = onProductNameChange(() => {
      changes += 1;
    });
    setProductName("Jarvis 4");
    expect(productName()).toBe("Jarvis 4");
    // a deployment that is not YYC AI never says "YYC AI" (Deyao's own chat said it in its message box)
    expect(withProductName("Message YYC AI")).toBe("Message Jarvis 4");
    expect(withProductName("在對話中告訴 YYC AI 即可修改或取消。")).toBe("在對話中告訴 Jarvis 4 即可修改或取消。");
    expect(changes).toBe(1);
    // the same name again, an empty one, something that is no name: nothing changes
    setProductName("Jarvis 4");
    setProductName("  ");
    setProductName(undefined);
    expect(productName()).toBe("Jarvis 4");
    expect(changes).toBe(1);
    off();
  });

  it("starts as the page's name on the web, the build's in a native app, YYC AI otherwise", () => {
    expect(initialProductName("Jarvis 4", "YYC AI")).toBe("Jarvis 4");
    // a native app has no page: its sign-in screen names the product its build was made for
    expect(initialProductName(null, "Jarvis 4")).toBe("Jarvis 4");
    expect(initialProductName(null, "  ")).toBe("YYC AI");
    expect(initialProductName(null, undefined)).toBe("YYC AI");
  });
});
