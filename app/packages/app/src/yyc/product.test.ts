import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_PRODUCT_NAME,
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
});
