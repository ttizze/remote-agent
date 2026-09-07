defmodule RemoteAgentServerWeb.ChannelCase do
  use ExUnit.CaseTemplate

  using do
    quote do
      @endpoint RemoteAgentServerWeb.Endpoint

      import Phoenix.ChannelTest
    end
  end

  setup _tags do
    :ok
  end
end
