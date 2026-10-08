// The complete driver snapshot includes offscreen elements. Preserve XCTest's
// existence assertions rather than substituting a viewport visibility check.
const response = http.post(BEX_IOS_DRIVER_URL + "/viewHierarchy", {
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ appIds: ["dev.remoteagent.mobile.ios"], excludeKeyboardElements: false })
});
if (response.status !== 200) {
    throw new Error("The iOS driver could not read the accessibility hierarchy");
}
const root = JSON.parse(response.body).axElement;
if (!root) {
    throw new Error("The iOS driver returned no accessibility hierarchy");
}
const forbidden = typeof ABSENT_ID === "undefined" ? null : new RegExp(ABSENT_ID);
const forbiddenType = typeof ABSENT_ELEMENT_TYPE === "undefined" ? null : Number(ABSENT_ELEMENT_TYPE);
if (!forbidden && forbiddenType === null) {
    throw new Error("An absent element identifier or type is required");
}
const pending = [root];
while (pending.length) {
    const element = pending.pop();
    if ((forbidden && forbidden.test(element.identifier || "")) || element.elementType === forbiddenType) {
        throw new Error("An unexpected accessibility element exists");
    }
    (element.children || []).forEach(function (child) { pending.push(child); });
}
