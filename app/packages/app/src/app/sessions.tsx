import { HostRouteBootstrapBoundary } from "@/components/host-route-bootstrap-boundary";
import { SessionsScreen } from "@/screens/lazy-screens";

export default function SessionsRoute() {
  return (
    <HostRouteBootstrapBoundary>
      <SessionsScreen />
    </HostRouteBootstrapBoundary>
  );
}
