defmodule RemoteAgentServer.MixProject do
  use Mix.Project

  def project do
    [
      app: :remote_agent_server,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      test_ignore_filters: [&String.starts_with?(&1, "test/support/")],
      deps: deps(),
      aliases: [
        quality: [
          "format --check-formatted",
          "compile --warnings-as-errors",
          "credo --strict",
          "dialyzer"
        ]
      ]
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
      {:credo, "~> 1.7", only: [:dev, :test], runtime: false},
      {:dialyxir, "~> 1.4", only: [:dev, :test], runtime: false},
      {:jason, "~> 1.4"},
      {:phoenix, "~> 1.8.13"}
    ]
  end
end
