defmodule RemoteAgentServer.MixProject do
  use Mix.Project

  def project do
    [
      app: :remote_agent_server,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      test_ignore_filters: [&String.starts_with?(&1, "test/support/")],
      deps: deps()
    ]
  end

  def application do
    [
      mod: {RemoteAgentServer.Application, []},
      extra_applications: [:logger]
    ]
  end

  defp deps do
    [
      {:bandit, "~> 1.12"},
      {:jason, "~> 1.4"},
      {:phoenix, "~> 1.8.13"}
    ]
  end
end
