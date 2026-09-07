defmodule RemoteAgentServer.Application do
  @moduledoc false

  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Phoenix.PubSub, name: RemoteAgentServer.PubSub},
      RemoteAgentServer.RelayRoutes,
      RemoteAgentServerWeb.Endpoint
    ]

    opts = [strategy: :one_for_one, name: RemoteAgentServer.Supervisor]
    Supervisor.start_link(children, opts)
  end

  @impl true
  def config_change(changed, _new, removed) do
    RemoteAgentServerWeb.Endpoint.config_change(changed, removed)
    :ok
  end
end
