import Svg, { Polygon } from "react-native-svg";

interface YycLogoProps {
  size?: number;
  /** One colour for every face (masks, monochrome icons); YYC's own colours when omitted. */
  color?: string;
}

export const YYC_LOGO_VIEWBOX = "0 0 512 512";

export const YYC_LOGO_FACES = [
  { points: "233.16,105.56 233.16,248.81 109.10,320.44 109.10,177.19", fill: "#02A9C4" },
  { points: "278.84,105.56 402.90,177.19 402.90,320.44 278.84,248.81", fill: "#0A7F95" },
  { points: "256.00,288.37 380.07,360.00 256.00,431.63 131.93,360.00", fill: "#E5A711" },
];

/** The three faces, for use inside an <Svg> with YYC_LOGO_VIEWBOX. */
export function YycLogoFaces({ color }: { color?: string }) {
  return (
    <>
      {YYC_LOGO_FACES.map((face) => (
        <Polygon
          key={face.fill}
          points={face.points}
          fill={color ?? face.fill}
          stroke={color ?? face.fill}
          strokeWidth={14}
          strokeLinejoin="round"
        />
      ))}
    </>
  );
}

// YYC's logo (assets/images/yyc-logo.svg) as a component, in place of Paseo's.
export function YycLogo({ size = 64, color }: YycLogoProps) {
  return (
    <Svg width={size} height={size} viewBox={YYC_LOGO_VIEWBOX}>
      <YycLogoFaces color={color} />
    </Svg>
  );
}
