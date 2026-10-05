// Keep invitations out of Maestro's evaluated inputText commands and reports.
// The pinned iOS driver receives the text directly over its loopback API.
if (REFRESH_PAIRING === "true") {
    const response = http.get(BEX_PAIRING_URL);
    if (response.status !== 200 || !response.body) {
        throw new Error("The isolated Host did not return a pairing invitation");
    }
    output.pairingInvitation = response.body.replace(/[\u0080-\uffff]/g, function (unit) {
        return "\\u" + unit.charCodeAt(0).toString(16).padStart(4, "0");
    });
}
if (!output.pairingInvitation) {
    throw new Error("A pairing invitation must be fetched before reuse");
}
const input = http.post(BEX_IOS_DRIVER_URL + "/inputText", {
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text: output.pairingInvitation, appIds: ["com.ttizze.b-codex"] })
});
if (input.status !== 200) {
    throw new Error("The iOS driver could not enter the pairing invitation");
}
