import { Icon } from "../components/Icon.js";
import { environmentLabel, SERVER_STATUS_LABEL } from "../lib/labels.js";
import { useServers } from "../lib/servers.js";
import { useWorkspaceStore } from "../stores/workspace-store.js";

/** Target identity stays visible while the inspector's page scrolls. */
export function ServerInspectorHeader({ onBack, onClose }: { onBack: () => void; onClose: () => void }) {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const servers = useServers();
  const server = servers.data?.find((item) => item.id === selectedServerId);

  return (
    <header className="inspector-object-header" aria-label="当前服务器">
      <div className="inspector-object-navigation">
        <button type="button" className="text-button inspector-back" onClick={onBack}>
          <Icon name="chevronRight" size="xs" /><span>所有服务器</span>
        </button>
        {server ? <span className={`inspector-connection inspector-connection-${server.status}`}>
          <span aria-hidden="true" />{SERVER_STATUS_LABEL[server.status]}
        </span> : null}
        <button type="button" className="icon-button context-close" aria-label="回到对话" onClick={onClose}><Icon name="close" /></button>
      </div>
      {server ? <div className="inspector-object-identity">
        <h2>{server.name}</h2>
        <div className="inspector-object-environment">
          <span className={`inspector-environment inspector-environment-${server.metadata.environment}`}>
            {environmentLabel(server.metadata.environment)}
          </span>
          {server.metadata.region ? <span>{server.metadata.region}</span> : null}
        </div>
        <p className="inspector-object-address"><code>{server.connection.username}@{server.connection.host}:{server.connection.port}</code></p>
      </div> : null}
    </header>
  );
}
