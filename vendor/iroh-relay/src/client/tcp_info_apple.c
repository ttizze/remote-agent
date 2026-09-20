// Read Darwin's SDK-defined ABI, including its packed TFO bitfields.
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stddef.h>
#include <stdint.h>
#include <errno.h>

int bex_relay_tcp_info(int fd, uint64_t values[11]) {
    struct tcp_connection_info info = {0};
    socklen_t length = sizeof(info);
    if (getsockopt(fd, IPPROTO_TCP, TCP_CONNECTION_INFO, &info, &length) != 0)
        return errno;
    if (length < offsetof(struct tcp_connection_info, tcpi_txretransmitpackets) + sizeof(info.tcpi_txretransmitpackets))
        return EOVERFLOW;
    values[0] = (uint64_t)info.tcpi_rttcur * 1000;
    values[1] = (uint64_t)info.tcpi_srtt * 1000;
    values[2] = (uint64_t)info.tcpi_rto * 1000;
    values[3] = info.tcpi_snd_sbbytes;
    values[4] = info.tcpi_txbytes;
    values[5] = info.tcpi_rxbytes;
    values[6] = info.tcpi_txretransmitbytes;
    values[7] = info.tcpi_rxoutoforderbytes;
    values[8] = info.tcpi_txretransmitpackets;
    values[9] = info.tcpi_snd_cwnd;
    values[10] = info.tcpi_snd_wnd;
    return 0;
}
