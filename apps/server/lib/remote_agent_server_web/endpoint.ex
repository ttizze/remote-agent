defmodule RemoteAgentServerWeb.Endpoint do
  @moduledoc false

  use Phoenix.Endpoint, otp_app: :remote_agent_server

  socket("/socket", RemoteAgentServerWeb.UserSocket,
    websocket: [max_frame_size: 100_000],
    longpoll: false
  )
end
