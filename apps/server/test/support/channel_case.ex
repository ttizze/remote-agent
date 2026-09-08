defmodule RemoteAgentServerWeb.ChannelCase do
  @moduledoc "Shared endpoint and Phoenix channel helpers for relay tests."
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
