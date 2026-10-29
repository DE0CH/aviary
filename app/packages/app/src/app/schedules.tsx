import { HostRouteBootstrapBoundary } from "@/components/host-route-bootstrap-boundary";
import { SchedulesScreen } from "@/screens/lazy-screens";

export default function SchedulesRoute() {
  return (
    <HostRouteBootstrapBoundary>
      <SchedulesScreen />
    </HostRouteBootstrapBoundary>
  );
}
