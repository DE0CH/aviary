import { PairScanScreen } from "@/screens/lazy-screens";

// The QR scanner (with the camera module) is its own chunk, fetched when the page opens (docs/paseo-ui.md "Loading")
export default function PairScanRoute() {
  return <PairScanScreen />;
}
