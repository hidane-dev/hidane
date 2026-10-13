// A Firestore-triggered function (2nd gen): when a message is created, it writes the message's
// text in upper case into the same document.
import { initializeApp } from 'firebase-admin/app';
import { onDocumentCreated } from 'firebase-functions/v2/firestore';

initializeApp();

export const uppercase = onDocumentCreated('messages/{id}', (event) => {
  const text = event.data.get('text');
  return event.data.ref.update({ uppercase: text.toUpperCase() });
});
