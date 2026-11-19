import { useMemo, useSyncExternalStore } from "react";
import { Image, type ImageStyle, type StyleProp } from "react-native";
import { isNative } from "@/constants/platform";
import { accessHeaders, accessToken, subscribeAccessToken } from "@/yyc/access-token";
import { brandAssetUrl } from "@/yyc/brand";
import { brandLogoSource, type BrandLogoSource } from "@/yyc/native-brand";
import { bundledBrandLogo } from "@/yyc/native-brand-assets";

/** The logo to draw: the server's, or in a native app nobody is signed in to, the build's own (yyc/native-brand.ts). */
function useBrandLogoSource(): BrandLogoSource {
  const uri = brandAssetUrl("logo.png");
  const token = useSyncExternalStore(subscribeAccessToken, accessToken, accessToken);
  return useMemo(
    () =>
      brandLogoSource({
        native: isNative,
        uri,
        headers: token ? accessHeaders() : {},
        bundled: bundledBrandLogo(),
      }),
    [uri, token],
  );
}

/** The deployment's logo at the size its style gives it. */
export function BrandLogoImage({ style }: { style: StyleProp<ImageStyle> }) {
  const source = useBrandLogoSource();
  if (!source) return null;
  return <Image source={source} style={style} resizeMode="contain" accessibilityIgnoresInvertColors />;
}

interface ProductLogoProps {
  size?: number;
}

/** The deployment's logo (`/branding/logo.png` of the server: yyc/brand.ts); nothing when the server is not known. */
export function ProductLogo({ size = 64 }: ProductLogoProps) {
  const source = useBrandLogoSource();
  const style = useMemo(() => ({ width: size, height: size }), [size]);
  if (!source) return null;
  return (
    <Image
      source={source}
      style={style}
      resizeMode="contain"
      accessibilityIgnoresInvertColors
      testID="product-logo"
    />
  );
}
