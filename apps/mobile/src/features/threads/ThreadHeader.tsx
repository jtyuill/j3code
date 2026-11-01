import { AppBar } from "../../components/ui/AppBar";
import { useThreadHeaderOptions } from "./useThreadHeaderOptions";

export function ThreadHeader(props: Parameters<typeof useThreadHeaderOptions>[0]) {
  const native = useThreadHeaderOptions(props);

  return (
    <AppBar
      title={props.title}
      subtitle={props.subtitle}
      sidebar={native.sidebar}
      options={native.options}
      optionsVersion={native.optionsVersion}
    />
  );
}
