defmodule RemoteAgentServerWeb.RunnerChannel do
  @moduledoc false
  use RemoteAgentServerWeb, :channel

  alias RemoteAgentServer.RelayRoutes

  @max_encoded_bytes 65_536
  @max_transport_messages 128

  @impl true
  def join("runner:" <> runner_id, _payload, %{assigns: %{runner_id: runner_id}} = socket) do
    case RelayRoutes.join(runner_id, socket.assigns.role) do
      {:ok, client_id} ->
        monitor = Process.monitor(Process.whereis(RelayRoutes))
        {:ok, %{clientId: client_id}, assign(socket, :routes_monitor, monitor)}

      {:error, reason} ->
        {:error, %{reason: reason}}
    end
  end

  def join(_topic, _payload, _socket), do: {:error, %{reason: "unauthorized"}}

  @impl true
  def handle_in("data", %{"data" => data} = payload, socket)
      when is_binary(data) and byte_size(data) in 1..@max_encoded_bytes do
    # Decode only to reject malformed envelopes; never inspect encrypted bytes.
    # Keep the original base64 text so forwarding does not re-encode/copy it.
    case Base.decode64(data) do
      {:ok, _bytes} ->
        reply_result(RelayRoutes.forward(Map.get(payload, "clientId"), data), socket)

      :error ->
        reply_result({:error, "invalid_payload"}, socket)
    end
  end

  def handle_in("close", %{"clientId" => id}, %{assigns: %{role: "runner"}} = socket)
      when is_binary(id) do
    reply_result(RelayRoutes.close_client(id), socket)
  end

  def handle_in("ack", payload, socket) when is_map(payload),
    do: reply_result(RelayRoutes.forward(Map.get(payload, "clientId"), :ack), socket)

  def handle_in(_event, _payload, socket), do: reply_result({:error, "invalid_payload"}, socket)

  @impl true
  def handle_info({:relay_open, id}, socket), do: bounded_push(socket, "open", %{clientId: id})
  def handle_info({:relay_close, id}, socket), do: bounded_push(socket, "close", %{clientId: id})

  def handle_info({:relay_ack, id}, %{assigns: %{role: "runner"}} = socket),
    do: bounded_push(socket, "ack", %{clientId: id})

  def handle_info({:relay_ack, _id}, socket), do: bounded_push(socket, "ack", %{})

  def handle_info({:relay_data, id, data}, %{assigns: %{role: "runner"}} = socket),
    do: bounded_push(socket, "data", %{clientId: id, data: data})

  def handle_info({:relay_data, _id, data}, socket),
    do: bounded_push(socket, "data", %{data: data})

  def handle_info({:relay_closed, reason}, socket) do
    push(socket, "closed", %{reason: reason})
    {:stop, :normal, socket}
  end

  def handle_info(
        {:DOWN, ref, :process, _pid, _reason},
        %{assigns: %{routes_monitor: ref}} = socket
      ),
      do: {:stop, :normal, socket}

  @impl true
  def terminate(_reason, _socket) do
    # The monitor handles abnormal exits too. Explicit leave releases the runner
    # before a clean reconnect, without waiting for a later DOWN message.
    try do
      RelayRoutes.leave()
    catch
      :exit, _ -> :ok
    end

    :ok
  end

  defp bounded_push(socket, event, payload) do
    case Process.info(socket.transport_pid, :message_queue_len) do
      {:message_queue_len, count} when count < @max_transport_messages ->
        push(socket, event, payload)
        {:noreply, socket}

      _ ->
        {:stop, :normal, socket}
    end
  end

  defp reply_result(:ok, socket), do: {:reply, :ok, socket}
  defp reply_result({:error, reason}, socket), do: {:reply, {:error, %{reason: reason}}, socket}
end
