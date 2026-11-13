import { useMemo } from "react";
import { Image, type ImageStyle, type StyleProp } from "react-native";
import { brandAssetUrl } from "@/yyc/brand";

/** The deployment's logo at the size its style gives it. */
export function BrandLogoImage({ style }: { style: StyleProp<ImageStyle> }) {
  const uri = brandAssetUrl("logo.png");
  const source = useMemo(() => (uri ? { uri } : null), [uri]);
  if (!source) return null;
  return <Image source={source} style={style} resizeMode="contain" accessibilityIgnoresInvertColors />;
}

interface ProductLogoProps {
  size?: number;
}

/** The deployment's logo (`/branding/logo.png` of the server: yyc/brand.ts); nothing when the server is not known. */
export function ProductLogo({ size = 64 }: ProductLogoProps) {
  const uri = brandAssetUrl("logo.png");
  const source = useMemo(() => (uri ? { uri } : null), [uri]);
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
