import { onDocumentCreated } from "firebase-functions/v2/firestore";
import { onRequest } from "firebase-functions/v2/https";

export const onOrderCreated = onDocumentCreated("orders/{orderId}", async (event) => {
  console.log("new order", event.params.orderId);
});

export const health = onRequest((_req, res) => {
  res.send("ok");
});
