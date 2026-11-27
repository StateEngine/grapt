# Google Gemini

Open **Connect**, select **Google**, and enter an API key. Grapt checks the
native model listing and completes a small native generation before saving the
key in the credential store. Both the desktop and TUI use this connection.
The model selector shows the models returned by the connected account.

Requests use Gemini GenerateContent, including streamed text, native function
calls, images, implicit cache usage and thought-signature history. Reopening a
session retains the signed parts for the same model, endpoint and credential
generation. Imported tool history on Gemini 3 uses Google’s documented missing
signature sentinel. Generation settings use provider defaults.

This adapter has not completed a credential-backed request to Google. Local
wire-compatible checks are development validation; cloud account and regional
availability still depend on Google. Vertex AI and Google OAuth are separate
connection methods.
