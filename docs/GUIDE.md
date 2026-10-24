# Implementation guide: from no Meta account to an assistant that reads and sends Messenger messages

This guide takes you from nothing (no Meta developer account) to an AI assistant that reads and
sends Facebook Messenger messages through meta-mcp. WhatsApp is the second track (Part 7).

**How to read it.**

- Dashboard paths, button labels and policy wording come from Meta's own documentation, as
  documented on 2026-10-24. Nobody drove a live dashboard while this guide was written, so every
  label is "as documented", not "as observed", and Meta changes its dashboard often. Where a label
  may differ it says so. If a step does not match your screen, search the dashboard for the
  setting's name.
- **UNVERIFIED** means no first-party text settles the point. **CONFLICT** means two Meta pages
  disagree. Each one comes with the test that settles it.
- Steps are numbered and meant to be followed in order. A **Why** line explains a step that is not
  obvious.
- Commands assume `meta-mcp` is on your `PATH` (Part 4 installs it).
- Links to Meta pages are in Appendix C. A link ending in `.md` returns the raw Markdown of the page.

**Contents**

- [Part 0: Read this first](#part-0-read-this-first)
- [Part 1: Meta developer account and app](#part-1-meta-developer-account-and-app)
- [Part 2: Pages, roles and access](#part-2-pages-roles-and-access)
- [Part 3: Facebook Login for the wizard](#part-3-facebook-login-for-the-wizard)
- [Part 4: Run the wizard](#part-4-run-the-wizard)
- [Part 5: Messenger in practice](#part-5-messenger-in-practice)
- [Part 6: Webhooks](#part-6-webhooks)
- [Part 7: WhatsApp](#part-7-whatsapp)
- [Part 8: Going to production](#part-8-going-to-production)
- [Part 9: Troubleshooting](#part-9-troubleshooting)
- [Appendix A: Permission reference](#appendix-a-permission-reference)
- [Appendix B: Scopes requested by the wizard](#appendix-b-scopes-requested-by-the-wizard)
- [Appendix C: Meta documentation links](#appendix-c-meta-documentation-links)
- [Appendix D: Glossary](#appendix-d-glossary)

---

## Part 0: Read this first

### 0.1 What the Graph API can and cannot reach

meta-mcp talks to the Meta Graph API, and the Graph API reaches two things only: **Facebook Page
inboxes** (Messenger conversations of a Page you administer) and **WhatsApp Business numbers**
on the Cloud API.

| It can | It cannot |
|---|---|
| Read and send Messenger messages of a Page you manage, with that Page's access token | Read or send from a **personal Messenger account**. There is no Graph endpoint for it |
| Send WhatsApp messages from a business number, and receive them through webhooks | Use a **personal WhatsApp number** as it is. Meta: "To use an existing WhatsApp Messenger phone number with Cloud API, you must first delete your WhatsApp Messenger account." |
| Store every webhook event locally for the assistant to read | Fetch **WhatsApp history**. Meta: "There are no APIs for fetching historical webhook data, so capture and store webhook payloads accordingly." Messages are retained 30 days at most for Meta's own retransmission |
| List Messenger conversations and read recent messages by polling | Read Messenger messages older than the 20 most recent of a conversation: Meta documents that only those expose details (whether Page inboxes enforce it live is UNVERIFIED, Part 5.3) |
| Message people who messaged the Page first | Start a Messenger conversation: "A person must initiate the conversation." |
| | Join or read your existing WhatsApp groups (the Groups API creates new, invite-only groups) |
| | Replay old webhooks: Meta says "You will not be able to query historical webhook event notification data" |

Instagram direct messages use the same Send API shape, but meta-mcp 0.1.0 has no Instagram tools.
Instagram webhook payloads are stored as raw `other` events and the wizard can request the
Instagram scopes with `--instagram`.

### 0.2 Three personas

Pick the one that matches what you are doing now. You can move from A to B or add C later.

| | A. Solo developer | B. Going Live for real customers | C. WhatsApp |
|---|---|---|---|
| Goal | Your assistant reads and sends on your own Page | Customers with no role on your app reach the Page | A test number first, then a real business number |
| App mode | Development | Live ("Publish > Go live") | Development for the test number; Live for some webhooks |
| Who the Page can talk to | Only people with a role on the app (Administrator, Developer, Tester) | Unknown until the gate test (Part 2.5) | Up to 5 recipient numbers on the test number (SECONDARY: archived pages only) |
| Needs App Review | No | Possibly: see the CONFLICT in Part 2.5 | No for your own business data |
| Read | Parts 1, 2.1 to 2.3, 3 or the token shortcut, 4, 5 | Parts 2.4 to 2.9, 6, 8 | Part 7, then 6 and 8 |

**A. Solo developer, own Page, Development mode.** Everything works for people who hold a role
on your app: sign-in, finding your Pages, minting a Page token, subscribing webhooks, reading and
sending messages. Real customers who have no role are out of reach until the app gets Advanced
Access, and in Development mode webhooks fire only for role users. This is the fastest way to get
a working assistant and is the path the guide assumes by default.

**B. Going Live to reach real customers.** You publish the app, run the gate test, and, if Meta
requires it, obtain Advanced Access on `pages_messaging` through Business Verification and App
Review. You also take on the policies in 0.4. Meta's own documents disagree on whether an app for
your own Page needs this (Part 2.5), so the guide has you test before you apply.

**C. WhatsApp test number, then a real number.** Meta creates a test number for you, with a
temporary token and a `hello_world` template. You then create a System User token, put the
webhook behind public HTTPS (inbound WhatsApp exists only through webhooks), and later register a
real number, add a payment method and meet the messaging limits.

### 0.3 Time and cost expectations

**What waits on Meta.** App Review is queued and decided by Meta. Business Verification asks for
documents that Meta's fetched pages do not list (UNVERIFIED, see Part 2.7). Template approval and
display-name approval are decided by Meta. None of these has a duration this guide can promise.

**What waits on you.** A public HTTPS address for the webhook, and a tunnel or reverse proxy to
provide it (Part 6). Messenger can be polled without one; WhatsApp cannot.

**What recurs.** The user token lasts about 60 days and needs `meta-mcp token refresh`. The Data
Use Checkup is annual. An app that sees no logins, API calls or webhooks for 90 days becomes
inactive and all its access tokens are invalidated. Part 8 has a schedule.

**Cost.**

- **Messenger.** The Messenger pages fetched for this guide list rate limits, not prices, for
  replies inside the standard window. No per-message fee for them appears in those pages
  (absence only: UNVERIFIED that none applies). Check Meta's current Messenger documentation
  before you rely on it.
- **WhatsApp.** Per-message pricing has applied since 2025-07-01, and Meta publishes USD. On the North
  America rate card effective 2026-10-01, marketing templates cost USD 0.0250 (CAD 0.0350) per delivered
  message and utility, authentication and service messages USD 0.0034 (CAD 0.00476), with the first
  1,000 service messages per business phone number per month free. **A payment method is still
  required.** The full table, the conversion basis and the billing rules are in Part 7.12.
- **Everything else.** A tunnel, hosting, and the language-model provider behind your assistant
  are your own costs and are not covered here.

### 0.4 Policies that bite an AI assistant

This is a summary of what Meta's own documents say, quoted where wording matters. **It is not
legal advice.** Read the documents (Appendix C), and note that Meta decides how its terms apply.

**WhatsApp: the AI Providers clause.** Section 4.7 of Meta's terms for the WhatsApp Business
Platform (Meta's pricing page says the update took effect on 2026-01-15) reads:

> Providers and developers of artificial intelligence or machine learning technologies, including
> but not limited to large language models, generative artificial intelligence platforms,
> general-purpose artificial intelligence assistants, or similar technologies as determined by Meta
> in its sole discretion ("AI Providers"), are strictly prohibited from accessing or using the
> WhatsApp Business Platform, whether directly or indirectly, for the purposes of providing,
> delivering, offering, selling, or otherwise making available such technologies when such
> technologies are the primary (rather than incidental or ancillary) functionality being made
> available for use, as determined by Meta in its sole discretion; provided, however, that such
> technologies may be made available to businesses in certain countries as set forth here.

The research reading: a one-owner server that lets an assistant send and read that owner's own
business messages is not obviously "providing" an assistant to others, but a number where third
parties chat with an AI assistant could be judged to fall under the clause, and the judgment is
Meta's alone. Settle it for your own case before you put an assistant on a business number.

**WhatsApp: sharing data with a language-model vendor.** Section 4.1 of the same terms says "You
must not ... (b) share, transfer, sell, license, or distribute WhatsApp Business Platform Data ...
to any third parties, except to a Solution Provider in accordance with these Terms". Section
4.1(a) bars using the data "to track, build, or augment profiles on individual WhatsApp Users",
with an exception only for "the content of your message threads". Section 4.7 also bars using
platform data to "create, develop, train, or improve" AI models, except fine-tuning a model for
your exclusive use. Sending inbound message text to a hosted model could be read as sharing it with
a third party.

**WhatsApp: automation and escalation.** The Business Messaging Policy says you "may use automation
when responding during the 24-hour window, but must also have available prompt, clear, and direct
escalation paths" (an in-chat human agent, a phone number, email, web support, in-store or a
support form). Opt-in is required, conversations may be started only with approved templates, and
you "may not forward or otherwise share information from a customer chat with any other customer."

**Messenger: disclose the automation.** Meta's Messenger policy says:

> When required by applicable law, automated chat experiences must disclose that a person is
> interacting with an automated service:
> - at the beginning of any conversation or message thread,
> - after a significant lapse of time, or
> - when a chat moves from human interaction to automated experience.

It singles out the California and German markets and adds: "Even where not legally required, we
recommend informing users when they're interacting with an automated chat as best practice". The
best-practices page adds "do not disguise an automated interaction as a real person."

**Messenger: other rules that apply to an assistant.**

- "Automated bots must respond to user input within 30 seconds." Use the typing indicator
  (`messenger_sender_action`) to cover a slow model.
- The `HUMAN_AGENT` tag lists "Automated messages" under disallowed usages, so it is not a way to
  send AI-written replies outside the window.
- Only contact people with legally sufficient opt-in and honour opt-outs; spam, including bots that
  message at very high frequencies, is not allowed; keep a Page with accurate support contact
  information.
- Platform Terms 3.c let you share data "with your Service Provider in order for them to provide
  the services you requested", and 5.a requires that provider to agree in writing to process it
  "solely for you and at your direction". Your privacy policy must describe it (Terms section 4).

**What to do about it.** Open every automated conversation by saying it is automated, offer a way
to reach a person, keep message content out of anything you have not disclosed in your privacy
policy, and prefer `META_MCP_READ_ONLY=true` until a human has approved what the assistant sends.

---


## Part 1: Meta developer account and app

By the end of this part you have a Meta app with the Messenger use case, its App ID and App
Secret, and the basic settings filled in. Everything here is documented in Meta's "Create an app"
guides; nothing needs a Page yet.

### 1.1 Register as a developer

1. While logged into Facebook, open `https://developers.facebook.com/async/registration` (or open
   `developers.facebook.com` and click **Get Started**).
2. Click **Next** to accept the Platform Terms and Developer Policies.
3. Verify. Meta says: "We will send a confirmation code to the phone number and email address that
   you provide in order to confirm that you have access to them."
4. Pick an occupation.

**Why:** Meta requires a registered developer account before you can create an app: "To create an
app with Meta, you must first register as a developer and be logged into your developer account."
The fetched page does not mention a government ID at registration (absence only). Identity checks
at the business level are a separate process (Part 2.7).

### 1.2 Create the app

1. Open `https://developers.facebook.com/apps/creation/`.
2. **App details:** enter the app name and a contact email, then **Next**.
3. **Use cases:** select one or more (next section), then **Next**.
4. **Business:** choose a business option (1.4), then **Next**.
5. **Requirements:** **Next**.
6. **Overview:** review, then click **Go to dashboard** to finish creating the app.

**Why:** this is the use-case-based creation flow. Meta says: "Use cases define the main ways your
app will interact with Meta's platform, such as authenticating users, accessing social features, or
managing business assets." Use cases cannot be removed after you create the app, so choose
deliberately.

### 1.3 Choose the Messenger use case

Select the use case Meta's docs name **"Engage with customers on Messenger from Meta"** (one page
writes it "Engage with customers on Messenger"). The exact tile text on the creation screen is
UNVERIFIED: the fetched docs show an empty use-case table. A use case named "Manage messaging and
content on Facebook Pages" was not found in any fetched document.

- Incompatible use cases are greyed out. The Pages use case cannot be combined with the Facebook
  Login use case. Whether Messenger can be combined with Facebook Login, with Pages, or with the
  WhatsApp use case is not documented in the fetched pages: UNVERIFIED. The creation screen is the
  test. meta-mcp holds **one** App ID and App Secret per configuration. If two products cannot
  share an app, run one meta-mcp instance per app, each with its own `META_MCP_HOME`.
- Do not pick the "Consumer" app type under **Other**: Meta says consumer apps "can't generate
  access tokens". App types still exist as an alternate path (select **Other**, then **Business**
  or **Consumer**). Business-type apps "do not have app modes and instead rely exclusively on access
  levels", and Meta's Access Levels page still carries the warning "This document is only
  applicable to apps created using an App Type."

**What the Messenger use case adds** (Messenger use-case guide):

| | Permissions |
|---|---|
| Required, cannot be removed | `business_management`, `pages_manage_metadata`, `pages_messaging`, `pages_show_list`, `public_profile` |
| Optional | `pages_read_engagement`, `instagram_basic`, `instagram_manage_messages`, `pages_utility_messaging`, `paid_marketing_messages`, `pages_user_gender`, `pages_user_locale`, `pages_user_timezone`, `ads_management`, `email`, and the Business Asset User Profile feature |
| Not offered at all | `pages_read_user_content`, `pages_manage_engagement` (comments, not messages) |

**Add `pages_read_engagement`.** In the dashboard, open the Messenger use case from the left menu
(**Dashboard**), click **Customize the Engage with customers on Messenger from Meta use case**,
confirm the required permissions are listed and click **Add** next to `pages_read_engagement`.

**Why:** the Page Conversations endpoint requires `pages_manage_metadata`, `pages_read_engagement`
and `pages_messaging`, and meta-mcp's `messenger_list_conversations` cannot work without all three.
`business_management` is a documented dependency of `pages_messaging` and `pages_show_list`; Meta
says to call that out in an App Review submission.

Then click **Ready to test** and make at least one successful API call per permission (the Graph
API Explorer works). Meta logs these calls; they are also a prerequisite for requesting Advanced
Access later.

### 1.4 Business portfolio choice

The **Business** step offers: "A verified business portfolio", "An unverified business portfolio",
"I don't want to connect a business portfolio yet.", or "Create a business portfolio".

- **Messenger on your own Page:** choose "I don't want to connect a business portfolio yet." Meta:
  "If you're building an app for a business you own or manage, you aren't required to connect a
  business to your app." You can connect one at any time during development.
- **WhatsApp, or a System User token for either product:** you need a business portfolio (System
  Users live in Business settings; the WhatsApp quickstart asks you to pick or create one).
- **Side effect:** "If your app is connected to a business portfolio, you must use the Business
  Manager to manage roles for your app."
- **Limits:** apps that serve other businesses must become a Tech Provider (out of scope). You may
  hold a developer or administrator role on at most 15 apps that are not connected to a Meta
  Verified Business Account.

### 1.5 Copy the App ID and the App Secret

1. In the dashboard open **Settings > Basic** (the left-menu label is "Settings" in the dashboard
   guide and "App Settings" in the security guide).
2. Copy the **App ID** (it is also in the dashboard toolbar).
3. Reveal and copy the **App Secret**. Store it in a password manager, not in a file in a
   repository.

**Why:** meta-mcp needs both. The App ID and App Secret together form the app access token
(`APP_ID` and `APP_SECRET` joined with a pipe) used to inspect tokens and to subscribe webhooks.
The App Secret also signs `appsecret_proof` and verifies webhook signatures. If it leaks, reset it
immediately in **Settings > Basic**. It is not possible to rotate it programmatically, and only an
Administrator can reset it (Part 8.7).

### 1.6 Settings needed before you publish

Under **Settings > Basic** Meta flags these as "required to switch your app to Live mode":
Display Name, Contact Email, Terms of Service URL, App Icon, Category and App Purpose. The Messenger
publish checklist adds a Privacy Policy URL, a data deletion URL (or callback), an icon "less than 5
MB, between 512 x 512 and 1024 x 1024 pixels, and in JPEG, GIF or PNG format", and data protection
officer contact information "if you are doing business in the European Union". The dashboard also
lists **User Data Deletion** here.

**Why:** you cannot go Live (Part 2.5) without them, and filling them in early avoids a stall later.
The Platform Terms require the privacy policy to "clearly explain what data you are Processing, how
you are Processing it, the purposes for which you are Processing it, and how Users may request
deletion of that data".

### 1.7 Leave "App Type" alone (the Native/Desktop gotcha)

Under **Settings > Advanced**, if **App Type** is set to **Native/Desktop**, Meta assumes the app
embeds its secret and "we do not allow calls signed with an App Access Token to proceed. The API
will behave as though no access token was provided." meta-mcp calls `debug_token` with the app
access token, so a Native/Desktop setting breaks it.

**Why:** the symptom is confusing (valid credentials rejected). If `meta-mcp setup` reports that the
App ID or App Secret is rejected and you are sure both are right, check this setting first.

---

## Part 2: Pages, roles and access

By the end of this part you know who may talk to your Page, what "Standard" and "Advanced" access
mean, and how to find out, with a test, whether your app needs App Review.

### 2.1 Page tasks: MESSAGING and MODERATE

The person who obtains the Page access token must be able to perform the **MESSAGING** and
**MODERATE** tasks on the Page. Meta: "The app user requesting the Page access token must be able
to perform the MESSAGING and MODERATE Page tasks." A full Page admin can do every task: "If a person
is given Admin access to a Page in the UI, that person is able to perform all tasks on that Page."

`MESSAGING` is "Send messages as the Page". Different endpoint pages list different task sets
(Conversations: `MESSAGING`; the Message node: `MODERATE` or `MESSAGING`; `subscribed_apps`:
`CREATE_CONTENT`, `MANAGE` or `MODERATE`), and an admin satisfies all of them.

**New Pages Experience.** The routing document names three ways to qualify: "Admin access or be
able to perform the MANAGE task on a Classic Facebook Page, Facebook access (New Pages Experience)
for the Facebook Page, or Full control via the Business Settings". Whether a partial task grant that
includes `MESSAGING` and `MODERATE` is enough on a New Pages Experience Page is UNVERIFIED. The test
is to request a token as such a user, or simply use a full admin.

**Why:** if the person lacks these tasks, the conversations and subscription calls fail with code
10, 200 or 230 (the wizard reports a permission or task problem and names the permissions).

### 2.2 App roles

| Role | What it can do |
|---|---|
| Administrator | All settings, can reset the app secret, remove the app, assign roles (up to 500) |
| Developer | Technical settings and insights |
| Tester | No settings access; "can grant the app any permission while it is in development, and all features are active for Testers" |
| Analytics user | Analytics only |

1. Open **App roles** (the Roles panel). The button labels (for example "Add People") are not named
   in the fetched docs: UNVERIFIED, look for the invite control.
2. Invite each person who will message your Page while testing with the **Tester** role. Tester
   invitations can go to registered developers and to regular Facebook users (most apps can have up
   to 50 testers).
3. A tester with no developer account accepts the invitation under Facebook **Settings & privacy >
   Your activity > Apps and Websites > Requests**, and agrees to the Meta Platform Terms.

**Why:** in Development mode only role users can reach the Page (next section). If the app is
connected to a business portfolio, manage roles in Business Manager instead.

**Test users are not an option.** Meta has paused creating new test users ("We are temporarily
removing the ability for apps to create new test users"), and existing test users cannot interact
with real Facebook users. Test with real accounts that hold the Tester role.

### 2.3 Who the Page can talk to in Development mode

Meta, Messenger quick start: "Until your app has been submitted and approved for public use on
Messenger through Meta's App Review, Page tokens only allow your Page to interact with Facebook
accounts that have been granted the Administrator, Developer, or Tester role for your app."

Three consequences you will see:

- **Sending to a non-role person fails** with code 200 (listed as subcode 2018028): "Cannot message
  users who are not admins, developers or testers of the app until pages_messaging permission is
  reviewed and the app is live."
- **Reading their conversations fails**: "Advanced Access is required to access conversations
  between your business and people who do not have a role on your messaging app, your Instagram
  Professional account, your Facebook Page, or your business".
- **Webhooks fire only for role users.** Standard Access gives you notifications from people who
  have a role on the app; Advanced Access (which requires App Review) is needed for customers.

People who only message the Page to test need an app role, not Page tasks.

### 2.4 Standard versus Advanced access

Meta's rules, verbatim:

> Permissions with Standard Access, however, can only be requested from app users who have a role
> on the requesting app, and features with Standard Access are only active for app users who have a
> role on the app.

> If your app will only be used by people who have a role on it, the permissions and features your
> app requires will only need Standard Access. If your app will be used by people who do not have a
> role on it, the permissions and features that your app requires will need Advanced Access.

> All Business, Consumer, and Gaming apps are automatically approved for Standard Access for all
> permissions and features. Advanced Access, however, must be approved on an individual permission
> and feature basis through the App Review process.

> However, Business Verification is required to get Advanced Access. In some cases additional App
> Review on an individual permission and feature basis might be required.

In a use-case dashboard the same idea shows as a status per permission: "Ready for testing",
"Verification required" ("This permission requires a verified Meta Business Account added to the
app") and "Ready for live mode" ("This permission has been approved in App Review and you can
publish your app"). To ask for more access, use **Actions > + Add to App Review** on a permission;
the dashboard also has an **Increase access** button for some permissions, and older text says
**Get Advanced Access**.

All seven permissions a Page inbox tool touches start at Standard Access. Appendix A lists them.

### 2.5 The CONFLICT, and the gate test that settles it

**CONFLICT: does an app for your own Page need App Review to reach real customers?**

Three first-party statements say no:

> App Review (if your app needs Advanced Access). Not required if you only send and receive messages
> for your own Facebook Page.

> If you are developing the app to send and receive messages for your own page, you will not have to
> go through app review.

> If you are building your app to access only data and assets you own, you don't need to submit your
> app for Meta App Review or publish your app. However, some products, such as webhooks, and some
> dashboard features, such as rate limit status, require you to publish your app.

Five say customers without a role need Advanced Access: the quick start's Page-token sentence (2.3),
the error table entry for code 200, the Conversations guide ("Advanced Access is required to access
conversations ..."), the webhook prerequisites ("Advanced Access to receive notifications from your
customers, people who do not have a role on your app. Requires App Review.") and the Message node's
"App Review" requirement. Meta's docs do not reconcile them. The effective rule is **UNVERIFIED in
practice**, so test it.

**The gate test.**

1. Finish the Basic settings of Part 1.6.
2. Publish the app: **Publish > Go live** (use-case apps), then click the **Go live** button in the
   bottom right corner. Meta also has a webhook prerequisite named "Publish your Meta app".
3. Use a Facebook account that has **no role** on the app and is not a Page admin. Have it message
   your Page.
4. Check that the message arrives: `inbox_list` shows it if the webhook is set up (Part 6), or
   `messenger_list_conversations` lists the conversation.
5. Reply with `messenger_send_text`.
6. Read the result:
   - The reply is delivered: Standard Access is enough for your Page. Stop here.
   - The reply fails with code 200, subcode 2018028: the app needs Advanced Access on
     `pages_messaging`. Continue with 2.6 and 2.7.

**Why:** Meta's own pages disagree, and only a real non-role message shows which statement governs
your app. The warning on going Live early: "This restriction applies to everyone, even users who
have a role on the app itself, so switching to Live mode prematurely may make your app unusable for
users who have a role on your app." That text is written for apps that have modes, and whether it
applies to use-case apps is UNVERIFIED. Publish only after the Basic settings are complete, and
expect Standard-Access behaviour afterwards. Whether a use-case app shows a Development/Live toggle
at all is UNVERIFIED (the toggle is documented only for app-type apps).

**The exception stated without conflict:** the user profile fields (first name, last name, picture)
need the **Business Asset User Profile Access** feature at Advanced Access even for your own Page.
`messenger_get_profile` returns a note instead of data without it.

### 2.6 What an App Review submission contains

You need this only if the gate test failed.

1. Make at least one successful API call with each permission you request. Meta: "Calls must be
   made within 30 days of submitting for App Review". The **Request advanced access** button stays
   grey until a call is logged, and logging can take up to 2 days. Graph API Explorer calls count.
2. In **App Review > Permissions and Features**, click **Request advanced access** per permission
   (use-case apps: **Actions > + Add to App Review**).
3. Complete Business Verification (2.7) and the data handling questions.
4. Complete the app settings: icon, privacy policy URL, category, primary contact and **App
   Purpose**: "Set this to Yourself or your own business if your app is only available to people who
   have a role on your app, or a role in a Business that has claimed your app. Otherwise set it to
   Clients."
5. Complete App Verification: tell Meta how to reach the app. "We will test your app using our own
   test accounts. Do not include your personal Meta Technologies app account's credentials."
6. For each permission, write a specific description (no copy and paste) and attach a screen
   recording (1080 or better, English interface, no audio needed).
7. Click **Submit for Review** and agree to the Platform Onboarding Terms. The submission is queued.

In a use-case dashboard the same flow reads: **Review > App Review > Edit**, review your app
settings, provide reviewer instructions, click each requested permission, tick the allowed-usage
box, then **Submit for Review**.

What the `pages_messaging` submission needs, from Meta's permission reference: a screencast that
demonstrates the complete Facebook login process; then "Demonstrate your app sending a message to a
Messenger user, and demonstrate the Messenger inbox client (either web or mobile app) receiving and
displaying the sent message"; and "Generate a cURL request that you can integrate into your app
platform to send a message" (the API Integration Helper in **Meta App Dashboard > Messenger** can
produce one). It must be a recording of the message being sent from the app, not screenshots. For
`business_management` as a dependency, name the main permission (`pages_messaging` or
`pages_show_list`) in the use-case description. Write a messaging-specific justification: the
permission reference's prompt for `pages_messaging` is a copy-paste error about ads.

Messenger-specific pre-checks: follow the Platform policies and Community Standards; "Publish the
Facebook Page associated with your app."; make sure your webhook returns 200 to events (the
checklist says "within 20 seconds", the webhooks guide says 5, so design for 5); and give reviewers
a way to reach any gated flow.

### 2.7 Business Verification

Meta: "Apps that request advanced access for permissions and apps that allow other Businesses to
access their own data must be connected to a Business that has completed Business Verification." And
for role-only apps: "If your app will only be used by app users who have a role on the app itself you
do not need to complete verification".

1. Open **Settings > Basic > Verification** and click **Start Verification**.
2. Anyone with the Administrator app role can connect a Business, but only a Business Admin can
   finish the verification.

Meta says individual verification is closed: "individual verification will no longer be allowed for
access once the business verification process is complete." What documents Meta asks for, and
whether a sole proprietor can pass, is UNVERIFIED (the Business Help Center article rendered
empty). The test is to start verification and read the form. The Human Agent and Business Asset
User Profile Access features are each "only available with business verification".

### 2.8 Data Use Checkup

Meta: "Data Use Checkup is an annual requirement whereby you, or another app admin, must certify that
your app still accesses our APIs and uses our products and data in compliance with our Platform Terms
and Developer Policies." Notices start 60 days before the deadline, then at 30, 10 and 3 days. "If
you do not complete DUC by your deadline, your app will be deactivated until DUC is completed.
Extensions will not be provided." It is not needed while the app is in Development mode, but "will
need to complete DUC before the app can be switched to Live mode". Whether a Standard-Access-only
published app owes it is ambiguous inside one Meta page (UNVERIFIED): treat it as required once the
app is published, and complete it from the dashboard banner or the **My Apps** "required actions".

### 2.9 Keeping the app alive

- **90-day permission dormancy:** "If your app does not use a permission for 90 days, usually due to
  user inactivity, your app user must regrant your app that permission."
- **Inactive apps:** an app with no logins, API calls or webhooks for 90 days is inactive, and "All
  access tokens associated with the app will be invalidated".
- **Invalidation without notice:** "Due to security related events, access tokens may be invalidated
  before the expected expiration time." and "Facebook will not notify you that an access token has
  become invalid." A password change or logout gives code 190 with subcode 460. meta-mcp treats code
  190 as "sign in again": run `meta-mcp setup` or `meta-mcp token refresh`.

---


## Part 3: Facebook Login for the wizard

`meta-mcp setup` can sign you in through Facebook Login in your browser, exchange the result for a
long-lived token and discover your Pages. This part registers the redirect address that sign-in
needs. **You can skip all of it** by pasting a token instead (3.5), at the price of doing the token
refresh yourself.

### 3.1 Which login product you have

- **Use-case apps (the 2026 default).** Facebook Login for Business (FLfB) is added automatically:
  "New business type apps and new apps created with a use case that supports Facebook Login for
  Business use it automatically." The dashboard path is the left menu **Facebook Login for Business
  > Settings**.
- **App-type apps and the classic product.** Under **Products**, click **Facebook Login**, then
  **Settings**, and check **Valid OAuth redirect URIs** in the **Client OAuth Settings** section.

Meta's 2026-08-27 "Login with Facebook open beta" post covers SDK features (one-tap sign-on,
iOS app switch, limited-login refresh) and does not change the browser-redirect web flow that
meta-mcp uses.

### 3.2 The redirect URI, and the two modes of `meta-mcp setup`

The dialog sends the browser back to a redirect URI, and Meta requires that URI to be registered.
meta-mcp offers two modes, and both print the exact URI to register and the dashboard path.

| Mode | Flag | Redirect URI (default) | What happens |
|---|---|---|---|
| `loopback` (default) | `--redirect-mode loopback` | `http://localhost:8766/callback` | meta-mcp listens on `127.0.0.1:8766`, checks `state`, shows "Signed in. You can close this tab." and stops listening |
| `paste` | `--redirect-mode paste` | `https://localhost:8766/callback` | No listener. The browser shows a connection error page, but its address bar holds the full redirect URL with `?code=...&state=...`. Copy it back into the terminal (the whole URL, or just the code) |

`--port N` changes the port (the default 8766 is fixed so the registered URI never changes) and
`--redirect-uri URI` overrides the URI in either mode. At code exchange the `redirect_uri` must be
byte-identical to the one in the dialog.

1. Open **Facebook Login for Business > Settings** (classic: **Products > Facebook Login >
   Settings**).
2. Add the exact URI above to the redirect URI list (**Valid OAuth Redirect URIs**) and click
   **Check URI**.
3. In **Client OAuth settings**, make sure **Client OAuth Login** and **Web OAuth Login** are on
   and **Strict Mode** is on. These toggle names are quoted from Meta's security guide; the live
   labels on this screen (for example "Use Strict Mode for redirect URIs") were not observed:
   UNVERIFIED, so look for the setting by meaning.
4. Add the **Deauthorize Callback URL** and the **Data Deletion Request URL**, then click **Save
   changes**.

**Why:** Meta: "Strict Mode prevents hijacking of your redirect URIs by requiring an exact match
from your Valid OAuth redirect URIs list." and "The full URI must be an exact match, including all
parameters, with the exception of the optional state parameter, the value of which is ignored."
So scheme, host, port, path and trailing slash all matter, and a random port is impossible: that is
why the wizard fixes the port. "Enforce HTTPS" is on by default ("All new apps created as of March
2018 have this setting on by default"), and "you can't request permissions for an access token if
you have Client OAuth Login disabled".

**UNVERIFIED: does Meta accept `http://localhost`?** No fetched Meta page mentions localhost for
OAuth redirects. Community reports (not Meta): `https://localhost:8000/...` worked with Strict Mode
on in Development mode, others say HTTPS enforcement cannot be disabled, and one report shows
"Insecure Login Blocked You can't get an access token or login to this app from an insecure page."
Nothing fetched covers Live mode. **The test that settles it:** add the candidate URI, press **Check
URI**, and run one real login in the mode you will use. If plain `http://localhost` is refused,
rerun the wizard with `--redirect-mode paste` (the URI is `https://localhost:8766/callback`), or use
a tunnel, or paste a token (3.5).

### 3.3 `config_id` (optional)

Facebook Login for Business can use a **configuration**: in the left menu **Facebook Login for
Business > Configurations > + Create configuration** (or **Create from template**), name it, choose
**User access token**, select the assets and permissions, click **Create**, and copy the
**Configuration ID**. Pass it with `meta-mcp setup --config-id ID`.

**Why:** Meta says "config_id has replaced scope (although scope can still be included, we
recommend that you do not use it)." When `--config-id` is given the wizard may omit `scope`. Without
it the wizard sends the scopes in Appendix B, which is how the manual flow works. Meta's Messenger
overview lists FLfB as "Required to request permissions from your app users to send and receive
messages on their behalf", while the FLfB FAQ says Facebook Login is recommended for consumer
sign-in if you are not a Tech Provider: a CONFLICT of emphasis. A use-case app has no separate
classic Login product, so plan on FLfB. A **System-user** configuration needs a business-portfolio
login, "can only request logins from web surfaces", and Meta says the app's portfolio "needs to be
separate from the business portfolio owned by your business client": that wording targets Tech
Providers and may block a solo developer (UNVERIFIED), so use the **User access token** type.

### 3.4 What the dialog asks for

The wizard builds `https://www.facebook.com/v26.0/dialog/oauth` with `client_id`, `redirect_uri`, a
`state` of 32 random bytes in hex, `response_type=code`, the scopes of Appendix B, plus
`auth_type=rerequest` when you pass `--rerequest` (it re-asks permissions the person declined). If
you cancel, Meta redirects with `error_reason=user_denied&error=access_denied`. The wizard then
exchanges the code for a short-lived token and the short-lived token for a long-lived one.

### 3.5 Paste a token instead of signing in

Three ways, from least to most durable.

**A. A Page token from the dashboard.** In the Messenger use case open **Messenger API Setup**, and
under **Generate access tokens** click **Connect**, log in, select the Page and continue (older
dashboards: the **Token Generation** section of **Messenger > Settings**, where the token appears in
the **Page Access Token** field). Meta: "The generated token is not saved in this UI. Each time you
select a Page from the dropdown, a new token will be generated. If a new token is generated,
previously created tokens will continue to function." Its lifetime is not stated: inspect it with
`debug_token` (UNVERIFIED). Then:

```bash
meta-mcp setup --app-id 123456789012345 --app-secret-env META_APP_SECRET --page-token "$PAGE_TOKEN" --non-interactive
```

The token ends up in your shell history and in the process list while the command runs. If that
matters, skip the wizard and use the environment instead (Part 4.9).

**B. A user token from the Graph API Explorer** (the Explorer is one of the three ways Meta lists for
getting a token). It is short-lived, about one to two hours. `meta-mcp setup --user-token TOKEN`
inspects it with `debug_token`, and when it expires within two hours and the app secret is known it
exchanges it for a long-lived token and tells you.

**C. A System User token** (no sign-in loop at all). As documented:

1. **Business settings > System Users > + Add**: give it a name and the Admin or Employee role.
2. Click the system user, then **Assign assets**, select your app and grant **Manage app** ("Full
   control").
3. Click **Generate token**, pick the app, a token expiration preference and the permissions
   (`business_management` is required when a system user is involved), and copy the token.
4. Give the system user the Page: assign it tasks on the Page, with `POST /{PAGE_ID}/assigned_users`
   and tasks `MANAGE`, `CREATE_CONTENT`, `MODERATE`, `ADVERTISE` or `ANALYZE` (`MESSAGING` is not in
   that list). Then fetch the Page token with `GET /me/accounts` using the system token. Run
   `meta-mcp setup --user-token SYSTEM_USER_TOKEN` and the wizard does that discovery.

Caveats, all UNVERIFIED for a messaging-only app: Meta's token types are "Non-expiring Access
Tokens ... Never expires" and "Suggested Expiring Access Tokens ... Valid for 60 days", and warns
"Some businesses must use expiring tokens" (whether the Business Settings UI still offers "never"
today is not known). The prerequisites include having the app go through App Review and Business
Verification for the permissions the system user wants, and the Marketing API wording says "To
create a system user access token, this app must have Standard Access." Whether a messaging-only app
satisfies that is not stated. Standard access allows 1 system user and 1 admin system user. Meta
also cautions that system-user tokens cannot be tied to a real person's actions "unless you take
them through Facebook Login". The test is to try it in Business settings.

**Not supported: device login.** Meta documents a device flow (`/device/login`), but it is not
documented whether it accepts the `pages_*` scopes, so meta-mcp does not use it.

---

## Part 4: Run the wizard

By the end of this part `credentials.json` holds a verified configuration, `meta-mcp doctor` is
green, and your MCP client can start the server.

### 4.1 Install

You need Rust 1.89 or newer (the crate's `rust-version`) and a C compiler: the bundled SQLite is
compiled from source.

```bash
git clone https://github.com/matpb/meta-mcp-rs.git
cd meta-mcp-rs
cargo install --path .
meta-mcp --version
```

`cargo install` puts `meta-mcp` in `~/.cargo/bin`. If a release binary is published for your
platform you can instead download it, make it executable and put it on your `PATH`. The last command
prints `meta-mcp 0.1.0`.

### 4.2 Run it

Interactive, the normal way:

```bash
meta-mcp setup
```

It asks for the App ID and App Secret (hidden input), opens your browser for the Facebook sign-in,
lets you pick Pages, offers WhatsApp and the webhook, and ends with the doctor report.
Non-interactive, for scripts (the secret comes from an environment variable, not from the command
line):

```bash
export META_APP_SECRET='replace-with-app-secret'
meta-mcp setup --app-id 123456789012345 --app-secret-env META_APP_SECRET \
  --login --no-browser --all-pages --non-interactive
```

With `--non-interactive`, or whenever stdin is not a terminal, a missing value is a usage error
that names the flag and exits with code 2.

### 4.3 Every flag

Precedence for every value: **flag, then environment** (`META_APP_ID`, `META_APP_SECRET`,
`META_WEBHOOK_PUBLIC_URL`, `META_WEBHOOK_VERIFY_TOKEN`), **then the existing `credentials.json`,
then an interactive prompt.**

| Flag | Meaning |
|---|---|
| `--app-id ID` | The Meta App ID |
| `--app-secret SECRET` | The App Secret. Accepted, but your shell history keeps it, and the wizard warns |
| `--app-secret-env NAME` | Read the App Secret from the environment variable `NAME` (recommended) |
| `--app-secret-file PATH` | Read the App Secret from a file |
| `--login` | Sign in with Facebook Login in the browser. The default when no token flag is given |
| `--user-token TOKEN` | Use a pasted user token (Graph API Explorer or System User) instead of signing in |
| `--page-token TOKEN` | Use a pasted Page token. No user token is stored |
| `--redirect-mode loopback\|paste` | `loopback` (default) or `paste` (Part 3.2) |
| `--redirect-uri URI` | Override the redirect URI in either mode |
| `--port N` | Loopback or paste port, default 8766. A busy port is an error that names the fix |
| `--config-id ID` | Facebook Login for Business configuration ID (Part 3.3) |
| `--rerequest` | Re-ask permissions the person declined earlier (`auth_type=rerequest`) |
| `--no-browser` | Print the dialog URL and do not open a browser |
| `--timeout-seconds N` | How long to wait for the sign-in callback, default 300 |
| `--page-id ID` | Select this Page; repeatable |
| `--all-pages` | Select every Page the user manages |
| `--no-messenger` | Skip Page discovery (a WhatsApp-only setup) |
| `--replace` | Replace the stored Pages instead of keeping those this run did not touch |
| `--whatsapp` | Also set up WhatsApp |
| `--whatsapp-token TOKEN` | A System User token for WhatsApp |
| `--waba-id ID` | The WhatsApp Business Account ID |
| `--phone-number-id ID` | A business phone number ID; repeatable |
| `--instagram` | Also request the Instagram scopes (`instagram_basic`, `instagram_manage_messages`) |
| `--webhook-url URL` | The public webhook URL to subscribe. Plain `http` is accepted for loopback hosts only |
| `--webhook-override` | Also register the URL as a WhatsApp override callback (Part 7.5) |
| `--verify-token TOKEN` | The webhook verify token; generated (32 random bytes in hex) when none exists |
| `--non-interactive` | Never prompt; a missing value exits with code 2 |
| `--yes` | Accept confirmation prompts |

### 4.4 What each step does and verifies

The wizard prints numbered steps with `✅` (done), `⚠️` (warning), `❌` (failed) and `ℹ️` lines,
no colour codes, and every `❌` is followed by a `Fix:` line.

| Step | What it does | What it verifies |
|---|---|---|
| 1. App | Resolves the App ID and App Secret | `GET /{app-id}?fields=id,name,link` with the app token proves the pair, and prints `✅ App <name> (<id>)`. On error 190 or 101: `❌ App ID or App Secret rejected: Settings > Basic in the App Dashboard` |
| 2. Token | Obtains a token (login, pasted user token, or pasted Page token) | Login: `state` is 32 random bytes in hex and is compared in constant time; the code is exchanged for a short-lived token and then a long-lived one; `debug_token` records scopes and expiry. Missing scopes print `⚠️ missing scope X: rerun with --rerequest`, which is fatal only when `pages_messaging` is missing in a Messenger setup. Pasted Page token: `debug_token` warns when `pages_messaging`, `pages_manage_metadata` or `pages_read_engagement` is missing, and `GET /me` names the Page |
| 3. Pages | Lists the Pages (`GET /me/accounts`, paginated), selects (flags or a numbered prompt) and stores each Page token | Each Page: `GET /{page-id}?fields=id,name` and `GET /{page-id}/conversations?limit=1&platform=messenger`. A result of code 10, 200 or 230 prints `⚠️` with the permission and task names |
| 4. WhatsApp | Discovers businesses, WhatsApp Business Accounts and phone numbers, or uses `--waba-id` and `--phone-number-id` | Each number: `GET /{phone-number-id}?fields=display_phone_number,verified_name,quality_rating,code_verification_status,status`. Warns that a user token expires in 60 days |
| 5. Webhook | Generates or reuses the verify token, checks reachability, subscribes at app and Page level, and for WhatsApp tries the app level and subscribes the WABA | The reachability self-check, then read-backs of the subscriptions. Detail in Part 6.4 |
| 6. Save | Writes `credentials.json` by merging into the existing file | Prints the path and its permissions |
| 7. Finish | Prints client snippets and runs the doctor | Exits with the doctor's code |

### 4.5 The `credentials.json` result

It lives in `META_MCP_HOME` (default: `~/.config/meta-mcp` on Linux, `~/Library/Application
Support/meta-mcp` on macOS, the roaming application data directory on Windows). It is written
atomically as mode 0600 in a directory of mode 0700, and meta-mcp warns when it finds it readable by
others.

```json
{
  "version": 1,
  "app_id": "123456789012345",
  "app_secret": "replace-with-app-secret",
  "user_token": {
    "token": "EAAreplace-with-long-lived-user-token",
    "expires_at": 1795000000,
    "scopes": ["pages_show_list", "pages_manage_metadata", "pages_messaging", "pages_read_engagement", "business_management"],
    "obtained_at": 1789800000
  },
  "pages": [
    {"id": "102938475610293", "name": "Example Shop", "token": "EAAreplace-with-page-token"}
  ],
  "whatsapp": {
    "waba_id": "102290129340398",
    "token": "EAAreplace-with-whatsapp-token",
    "token_kind": "system_user",
    "phone_numbers": [
      {"id": "106540352242922", "display_phone_number": "+1 555 010 0100", "verified_name": "Example Shop"}
    ]
  },
  "webhook": {"verify_token": "replace-with-random-hex", "public_url": "https://hooks.example.org/webhook"}
}
```

| Field | Meaning |
|---|---|
| `version` | Schema version, 1. A newer file than the build supports is refused |
| `app_id`, `app_secret` | Omitted when unknown |
| `user_token` | `null` when you used `--page-token`. `expires_at` is Unix seconds or `null`; `scopes` are what `debug_token` reported |
| `pages` | One entry per stored Page, each with its own non-expiring Page token (when derived from a long-lived user token) |
| `whatsapp` | `null` without WhatsApp. `token_kind` is `user` or `system_user` |
| `webhook` | The verify token and the public URL |

An empty `{}` file loads as an empty configuration.

### 4.6 `meta-mcp doctor`

```bash
meta-mcp doctor            # exit 0 when no check failed, 1 otherwise
meta-mcp doctor --json     # {"checks": [...], "ok": true|false}
meta-mcp doctor --webhook  # also GET the public callback URL
```

The summary line reads `N ok, N warnings, N failures`. Each check is a status (`✅`, `⚠️`, `❌`), a
detail, and for anything not green a `Fix:` line. Every network check has a 15-second timeout and
a failure never crashes the command. Tokens and the app secret are never printed. Line by line:

| Check | Calls and looks at | Warning when | Failure when |
|---|---|---|---|
| App | `GET /{app-id}?fields=id,name` with the app token; the mode of `credentials.json` when the file exists | The file's mode is not 0600 (the fix offers `chmod 600`) | The app token is rejected |
| User token | `debug_token`: validity, days left, scopes | Under 10 days left (fix: `meta-mcp token refresh`); a scope is missing | The token is invalid (fix: `meta-mcp setup`) |
| Each Page | `debug_token` (type, expiry), `GET /{page-id}?fields=id,name`, the conversations probe, `subscribed_apps` fields | A webhook is configured but `messages` is missing from the subscribed fields (fix: `meta-mcp setup --webhook-url ...`) | The token is invalid or a call fails; the line names `meta-mcp setup` |
| App subscriptions | `GET /{app-id}/subscriptions` | A public URL is stored but no `object=page` subscription matches it | The call fails |
| WhatsApp | The token with `debug_token`, each phone number, and the WABA's `subscribed_apps` (does it list this app) | | A call fails |
| Webhook | The stored public URL and verify token; with `--webhook`, our own verification GET against the callback | | With `--webhook`, the callback does not answer with the challenge |
| Inbox | Opens the database; counts and the age of the last event | A webhook is configured and no event arrived in 7 days ("send a test from the dashboard") | The database cannot be opened |
| Files directory | Configured and writable (information) | | |
| Read-only | The flag is reported (information) | | |

### 4.7 The token commands

```bash
meta-mcp token refresh              # re-exchange the stored user token and re-derive Page tokens
meta-mcp token refresh --whatsapp   # refresh the stored WhatsApp token the same way
meta-mcp token import --page-token TOKEN
meta-mcp token import --user-token TOKEN
meta-mcp token import --whatsapp-token TOKEN --phone-number-id ID --waba-id ID
meta-mcp token show                 # kinds, expiry, scopes, names; never a token value
meta-mcp token show --json
```

`token refresh` uses the stored app secret to trade the user token for a fresh long-lived one
(`fb_exchange_token`), stores the new token and expiry, and replaces the stored Page tokens of the
same Pages. With `--whatsapp` it refreshes the WhatsApp token with `set_token_expires_in_60_days=true`.
`token import` checks the value with `debug_token` when the app secret is known, then stores it.
`token show` prints at most `EAA` plus 4 characters of any token, and normally none.

**Lifetimes** (as documented): a short-lived user token lasts about 1 to 2 hours; a long-lived one
about 60 days ("You can not use an expired token to request a long-lived token"); Page tokens
derived from a long-lived user token "do not have an expiration date and only expire or are
invalidated under certain conditions" (derived from a short-lived one they last 1 hour); System User
tokens are 60-day or non-expiring. Error 190 means the token is dead: `meta-mcp setup` (new login)
or `meta-mcp token refresh` while the user token is still valid. The server reads
`credentials.json` at startup, so **restart it after a refresh**.

### 4.8 Client snippets

At the end the wizard prints the Claude Code command, a generic JSON snippet and the HTTP-mode command;
this section adds Claude Desktop and Cursor. After `meta-mcp setup`, the client only needs to start
`meta-mcp` (stdio); credentials come from `credentials.json`. If you chose a custom `META_MCP_HOME`,
pass it in the client's `env`.

**Claude Code.**

```bash
claude mcp add meta-mcp -- meta-mcp
claude mcp add --env META_MCP_READ_ONLY=true --transport stdio meta-mcp -- meta-mcp
claude mcp add --transport http meta-mcp https://meta.example.org/mcp --header "Authorization: Bearer YOUR_API_KEY"
```

The `--` separates Claude's options (`--transport`, `--env`, `--scope`) from the server command, and
Claude's docs say to put another option between `--env` and the server name. `--scope` takes
`local` (default), `project` or `user`. The third line is HTTP mode (Part 8).

**Claude Desktop.** Open **Settings > Developer > Edit Config**. The file is
`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS and
`%APPDATA%\Claude\claude_desktop_config.json` on Windows. Restart Claude Desktop afterwards.

```json
{
  "mcpServers": {
    "meta-mcp": {
      "command": "meta-mcp",
      "env": {"META_MCP_READ_ONLY": "true"}
    }
  }
}
```

**Cursor.** `.cursor/mcp.json` in a project, or `~/.cursor/mcp.json` for every project:

```json
{
  "mcpServers": {
    "meta-mcp": {"command": "meta-mcp"}
  }
}
```

A remote (HTTP mode) server in Cursor uses `"url": "https://meta.example.org/mcp"` with
`"headers": {"Authorization": "Bearer YOUR_API_KEY"}`.

**Any other MCP client.** The minimum is the command: `{"mcpServers": {"meta-mcp": {"command": "meta-mcp"}}}`.
Check your client's documentation for where the file lives.

### 4.9 Environment-only configuration (containers)

No wizard and no file are needed. Environment variables override `credentials.json`, and meta-mcp
never reads `.env` files, so inject them through your orchestrator or a systemd `EnvironmentFile`.

| Goal | Variables |
|---|---|
| Messenger | `META_PAGE_ACCESS_TOKEN` and `META_APP_SECRET`. The Page ID is read with `GET /me` at startup, or set `META_PAGE_ID` |
| WhatsApp | `META_WHATSAPP_ACCESS_TOKEN`, `META_WHATSAPP_PHONE_NUMBER_ID`, and `META_WABA_ID` |
| Webhook receiver | `META_APP_SECRET`, `META_WEBHOOK_VERIFY_TOKEN`; add `META_WEBHOOK_PUBLIC_URL` with `http --with-webhook` |
| HTTP mode | `META_MCP_API_KEYS`, `META_MCP_ALLOWED_HOSTS` (Part 8) |
| Persistence | `META_MCP_INBOX_DB` on a volume that survives restarts |

An environment Page token replaces the stored list of Pages with that one Page, and an environment
WhatsApp token replaces the stored WhatsApp block. With the app secret set, every call carries
`appsecret_proof`. The token commands work on `credentials.json`, so with environment-only
configuration you rotate the values yourself. Every variable is in `env.example`.

---


## Part 5: Messenger in practice

The tools below are described in full in [TOOLS.md](TOOLS.md). This part is about how they behave
against Meta's rules.

### 5.1 A first conversation, tool by tool

1. **Check the wiring.** `meta_status` shows each Page with `token_ok` and the webhook fields it is
   subscribed to.
2. **List conversations.** `messenger_list_conversations` returns newest activity first. Each entry
   has `counterpart.psid`, the customer's Page-scoped ID, which is the recipient for sends.
3. **Read one.** `messenger_read_conversation` with the conversation `id` returns messages newest
   first, with attachments and shared links.
4. **Reply.** `messenger_send_text` with `recipient_psid` (or `conversation_id`, which the tool
   resolves to the PSID: Meta offers no way to send to a conversation ID). Use `reply_to_mid` to
   quote a message.
5. **Attachments.** `messenger_send_attachment` sends an image, audio, video or file by URL or from a
   file in `META_MCP_FILES_DIR`. `messenger_download_attachment` saves one the customer sent. Meta
   documents no supported-format list for Messenger attachments (UNVERIFIED), and the docs do not say
   whether text and an attachment can share one message (UNVERIFIED), so send them as two calls.
6. **Sender actions.** `messenger_sender_action` with `typing_on` while the model thinks, `mark_seen`
   when you have read, `react` or `unreact` for reactions.
7. **Names.** The `name` in `counterpart` comes with the conversation. `messenger_get_profile` needs
   Advanced Access (5.6).

**Inbound attachments** arrive in webhooks typed `image` (including gif), `audio`, `video`, `file`,
`sticker`, `reel`, `ig_reel`, `post`, `ig_post`, `appointment_booking`, `fallback` or `template`.
"After August 30, 2026, only the sticker attachment type will be sent": a parser that expects
`image` for stickers will miss them. Whether attachment URLs need a token, and how long they last, is
undocumented (UNVERIFIED). Third-party samples show a signed expiry parameter (`oe`) that Meta does
not document, and Meta's pages say only that profile picture URLs "will expire". Download promptly,
and re-read the message by ID for fresh URLs.
meta-mcp tries the URL without a token and retries once with the Page token after a 401 or 403, for
Meta CDN hosts only.

### 5.2 The 24-hour window, and the dead tags

"For most user actions, it lasts 24 hours. When the user action is a person sending their first
message after clicking a Click-to-Messenger or Click-to-Direct ad, the window may last up to 7 days
from that message." A message to the Page, tapping Get Started, a reaction to a message, a comment
on a Page post and a visitor post all open it. "A person must initiate the conversation." Sending
outside the window fails (code 10, subcode 2018278, or 2534022) and nothing is sent.

| `messaging_type` | Meaning (Meta) | In meta-mcp |
|---|---|---|
| `RESPONSE` | A reply to a received message, promotional or not, inside the window | The default |
| `UPDATE` | Proactive, not a reply, still inside the window | Accepted |
| `MESSAGE_TAG` | Outside the window, non-promotional, with a matching `tag` | Requires `tag` |

| Tag | Status on 2026-10-24 | Rule |
|---|---|---|
| `CONFIRMED_EVENT_UPDATE`, `POST_PURCHASE_UPDATE`, `ACCOUNT_UPDATE` | Error 100 since 2026-04-27 | Meta: use Utility Templates or Marketing Messages |
| `CUSTOMER_FEEDBACK` | Listed | Customer Feedback Template only, within 7 days; not exposed by meta-mcp |
| `HUMAN_AGENT` | Listed | A human agent's reply within 7 days of the person's message; needs the Human Agent feature (App Review and Business Verification); lists "Automated messages" as disallowed |
| `NON_PROMOTIONAL_SUBSCRIPTION` | News Pages registered in the News Page Index only | Not exposed |

Meta: "Effective April 27th, 2026, all API requests containing the Message Tags
CONFIRMED_EVENT_UPDATE, ACCOUNT_UPDATE, and POST_PURCHASE_UPDATE will receive error code 100." The
Graph reference enum still lists the old values, so the enum lags behaviour.

**Do not use `HUMAN_AGENT` for AI-written replies.** It is for a person typing in a custom inbox.
Inside the window, AI replies are normal. Outside it, Meta's documented routes are utility messages
(a pre-approved template sent with `pages_utility_messaging`, which "must not contain marketing
materials"), Marketing Messages, One-Time Notification and Sponsored Messages. None of them has a
meta-mcp tool.

### 5.3 The 20-message detail limit

Meta: "Queries to the /CONVERSATION-ID endpoint will return all message IDs in a conversation.
However, you can only get details about the 20 most recent messages ... If you query a message that
is older than the last 20, you will see an error that the message has been deleted." (code 9000001,
"This Message has been deleted by the user or the business"). `messenger_read_conversation` adds a
`note` when you ask for more than 20, or when a message has neither text nor attachments.

Meta's Message and Conversation references repeat the limit under Instagram-only headings, so
whether Messenger Page inboxes enforce it live is **UNVERIFIED**. The test is to read a conversation
with more than 20 messages with `limit` 30 and look at the older entries. Either way, do not rely on
history: a webhook (Part 6) stores every message as it arrives, and Meta cannot replay old webhooks.
Also, Requests-folder conversations that have not been active for 30 days are not returned.

### 5.4 Rate limits

| Limit | Value |
|---|---|
| Conversations API | 2 calls per second per Page (meta-mcp spaces conversation reads at least 500 ms apart per Page) |
| Send API: text, links, reactions, stickers | 300 calls per second per Page |
| Send API: audio or video | 10 calls per second per Page |
| 24-hour cap | `200 * Number of Engaged Users` calls |
| One thread | "may be rate limited if too many messages are sent to a single thread" |
| High volume | Above 40 messages per second, calls that list all conversations fail; single-conversation reads still work |

Error codes: 4 (app limit), 17 (user token limit), 32, 613 (custom limit; 613/2534040 "Calls to this
api have exceeded the rate limit"), 80001 (Pages calls with a Page token), 80006 (Messenger: "too
many messenger api calls to this Page account"), 341 (application limit), 368 (temporarily blocked
for policy), 1 and 2 (temporary). Meta: "When the limit has been reached, stop making API calls.
Continuing to make calls will continue to increase your call count." meta-mcp retries reads twice
(400 ms, then 1200 ms, plus jitter) on 1, 2, 4, 17, 32, 613 and 80006, and never retries a send.
Meta publishes `X-App-Usage` and `X-Business-Use-Case-Usage` headers; meta-mcp does not surface
them. Whether reads other than the conversation list use the 2-per-second budget is UNVERIFIED.

### 5.5 Conversation routing and Page Inbox coexistence

Meta: "Meta no longer supports Handover Protocol for Messenger and all the businesses are migrated to
Conversation Routing." By default, "All connected applications receive messaging webhooks" and "All
applications can respond to the same user message without restrictions." Meta's own advice: "If only
one application is connected to your business and is solely responsible for receiving and responding
to messages, use default behavior. Coordination of responses from both the application and page
inbox is necessary to ensure no duplicate responses to the same message."

What that means for you:

- **Humans and the assistant can both answer.** Replies typed in the Page Inbox come to the webhook
  as `echo` events (the echo's `app_id` is the Page Inbox's). `inbox_list` with `kinds: ["echo"]`
  shows them, so the assistant can check before answering twice.
- **A default app changes the rules.** If you set one, "Only the default app can send a message when
  the thread is idle, and the conversation must be within the applicable messaging window for the
  app to message the user." Per-app settings are under **Page Settings > Page Setup > Advanced
  Messaging > Connected apps**, or **Meta Business Suite > Settings > Conversation routing**.
  `is_owner` appears on conversations once routing is enabled.
- **You do not need to be a "primary receiver"** to get message webhooks; leftover wording about
  primary receivers in the webhook reference appears to be legacy (the research's inference).
- **Meta Business AI.** "When Meta Business AI owns a conversation, your app can receive messages on
  the standby channel. Do not respond to standby messages. Start responding after your app receives a
  messaging_handovers webhook event containing pass_thread_control". The setting "Take control of
  conversations with your Business AI" is off by default, and when it is off an untagged send fails
  with code 10, subcode 2018300. meta-mcp stores `standby` events with kind `standby`.

### 5.6 The user profile needs Advanced Access

`messenger_get_profile` reads `first_name`, `last_name` and `profile_pic`. Those fields, and `id`
and `name`, need the **Business Asset User Profile Access** feature at Advanced Access ("Requires
App Review"; "only available with business verification"). `locale`, `timezone` and `gender` need
`pages_user_locale`, `pages_user_timezone` and `pages_user_gender`. Request them per Page under
**Page Settings > Advanced Messaging > Info About People**. Without access Graph answers with an
empty object, or code 2018247 ("Insufficient permission to access user profile."), or 2018218 ("No
profile available for this user.", for accounts created from a phone number). The picture URL
expires. Names from `messenger_list_conversations` come for free and are the better source.

---


## Part 6: Webhooks

### 6.1 Why optional for Messenger and mandatory for WhatsApp

- **Messenger can be polled.** `messenger_list_conversations` and `messenger_read_conversation` read
  the Page inbox on demand (2 calls per second per Page, newest first). A webhook adds push, replies
  typed in the Page Inbox (`echo`), deliveries and reads, and it keeps a local copy.
- **WhatsApp cannot be polled.** The Cloud API has no endpoint that lists or fetches messages, so
  inbound WhatsApp messages exist only because a webhook receiver stored them. Meta: "There are no
  APIs for fetching historical webhook data, so capture and store webhook payloads accordingly."
- **Meta never replays.** "You will not be able to query historical webhook event notification
  data, so be sure to capture and store any webhook payload content that you want to keep."

Meta's endpoint requirements: a public server with a valid TLS certificate ("Self-signed
certificates are not supported."), a `GET` verification handshake (`hub.mode`, `hub.verify_token`,
`hub.challenge`; answer with the challenge), `POST` events answered with 200, and payloads signed
with `X-Hub-Signature-256`. meta-mcp's receiver does all four.

### 6.2 Run the receiver

```bash
export META_APP_SECRET='replace-with-app-secret'
export META_WEBHOOK_VERIFY_TOKEN="$(openssl rand -hex 32)"
meta-mcp webhook
```

The receiver needs the app secret and a verify token, from the environment or from
`credentials.json`. Use the **same verify token** when you subscribe in 6.4: export
`META_WEBHOOK_VERIFY_TOKEN` in both places, or pass `--verify-token`. The receiver binds
`127.0.0.1:8787` (`META_MCP_WEBHOOK_LISTEN`) and writes the inbox database (`META_MCP_INBOX_DB`). It
refuses to start without both secrets and says where to set them. Check it locally:

```bash
curl "http://127.0.0.1:8787/webhook?hub.mode=subscribe&hub.verify_token=$META_WEBHOOK_VERIFY_TOKEN&hub.challenge=42"   # prints 42
curl http://127.0.0.1:8787/health    # {"events":0,"last_received_at":null,"ok":true,"unprocessed":0}
```

A wrong verify token gets 403 `verification failed`; a `POST` with a missing or wrong signature gets
401 `invalid signature` and nothing is stored.

### 6.3 Expose it over public HTTPS

The receiver listens on loopback and speaks plain HTTP, so put a TLS endpoint in front of it. Any of
these works. The vendors change their limits and terms: check their current documentation.

| Option | How | Notes |
|---|---|---|
| Cloudflare quick tunnel | `cloudflared tunnel --url http://127.0.0.1:8787` | Prints a `trycloudflare.com` URL; no account or domain needed. Cloudflare describes it as for testing and development, with no uptime guarantee, at most 200 in-flight requests, and a **new hostname on every run**, so re-subscribe (6.4) each time |
| Cloudflare named tunnel | Create a standard Cloudflare Tunnel with a hostname on a domain you control, routed to `http://127.0.0.1:8787` | Cloudflare recommends a standard tunnel for production; the hostname is stable |
| Tailscale Funnel | `tailscale funnel 8787` | Public URL on your `ts.net` name. Needs MagicDNS, HTTPS certificates enabled for the tailnet and the `funnel` node attribute; allowed ports 443, 8443 and 10000; bandwidth limits apply |
| ngrok | `ngrok config add-authtoken $YOUR_TOKEN`, then `ngrok http 8787` | Needs an account; prints an HTTPS forwarding URL. Whether the free address stays the same across restarts is not specified. Meta's own Messenger sample suggests ngrok for local webhooks |
| Reverse proxy with TLS | The nginx example in Part 8.2 | Your own certificate. The proxy must pass the `Host` header |

Your webhook URL is the public address plus `/webhook`, for example
`https://hooks.example.org/webhook`. Meta's source addresses change (they are in AS32934), so Meta
recommends mTLS over IP allow-lists; meta-mcp does not implement mTLS, and the signature check is its
gate. Meta changed the certificate authority for its webhook mTLS client certificate on 2026-03-31:
this matters only if your proxy requires and verifies it.

### 6.4 Subscribe: `meta-mcp setup --webhook-url`

```bash
meta-mcp setup --webhook-url https://hooks.example.org/webhook
```

With an existing `credentials.json` the wizard reuses the stored App ID and App Secret, and with
`META_WEBHOOK_VERIFY_TOKEN` exported it uses the token your receiver already has. It signs you in again
unless you pass `--user-token` or `--page-token`, then repeats the subscription steps below, which are
idempotent: run it as often as you need, for example after a long outage (6.5). Steps:

1. **Verify token.** `--verify-token`, else `META_WEBHOOK_VERIFY_TOKEN`, else the stored one, else a
   new 32-byte hex value.
2. **Reachability self-check.** It sends our own handshake to your URL (`GET` with `hub.mode`,
   `hub.verify_token` and a random `hub.challenge`, 10-second timeout) and needs 200 with the
   challenge as the body. If it fails, setup stops and tells you to start `meta-mcp webhook` and the
   tunnel, then rerun. **Why:** Meta sends the same `GET` while you subscribe, so the receiver must
   already be reachable. Plain `http` URLs are accepted for loopback hosts only.
3. **App level.** `POST /{app-id}/subscriptions` with the app token: `object=page`, `callback_url`,
   the field list, `verify_token`, `include_values=true`.
4. **Page level.** `POST /{page-id}/subscribed_apps` with the Page token and `subscribed_fields` set
   to the same list. Meta: "only fields with subscriptions at both the page and app levels will get
   Webhooks."
5. **WhatsApp**, when configured. It tries `POST /{app-id}/subscriptions` with
   `object=whatsapp_business_account`, `fields=messages`. Meta's docs describe that call, but its
   Graph reference says WhatsApp objects must be configured in the dashboard, so on any error the
   wizard prints the dashboard steps (Part 7.5). Then it subscribes the WABA: `POST
   /{waba-id}/subscribed_apps`, with `--webhook-override` also sending `override_callback_uri` and
   `verify_token`.
6. **Read-back.** It reads `GET /{app-id}/subscriptions` and `GET /{page-id}/subscribed_apps` and
   prints the fields that are active.

The field list: `messages`, `message_echoes`, `message_reads`, `message_deliveries`,
`message_reactions`, `message_edits`, `messaging_postbacks`, `messaging_referrals`,
`messaging_optins`, `messaging_handovers`, `standby` and `messaging_policy_enforcement` (Meta asks
you to subscribe to the last so you hear about violations). A text inbox needs `messages` and
`message_echoes` at minimum; that is the research's recommendation, not Meta's.

**The dashboard route** does the same job. In **Messenger API Setup** (older dashboards: **Products >
Messenger > Settings**): in **Configure webhooks** add the Callback URL and Verify token; in
**Generate access tokens** click **Connect**, select the Pages and click **Continue**, then **Save**
and **Got it**; click **Add Subscriptions**, tick each field and click **Confirm**. The labels "Add or
remove Pages" and "Generate token" appear in no fetched doc, so the live screen may differ
(UNVERIFIED). Meta adds: "Webhook notifications will only be sent if your Page has installed your
Webhooks configured-app, and if the Page has not disabled the App platform in its App Settings."

### 6.5 The 5-second and 1-hour rules

- **Answer fast.** Meta's webhooks guide says respond with 200 "within 5 or less seconds". The
  Messenger FAQ says "A webhook call with timeout after 20 seconds" and the App Review checklist says
  "within 20 seconds"; bots are separately held to 30 seconds to reply. **Design for 5.** meta-mcp's
  receiver verifies, stores and answers inside the request and does no other work.
- **One hour.** "If delivery of a notification continues to fail for 1 hour, you will receive a
  Webhooks Disabled alert, and your app will be unsubscribed from the webhooks for the Page or
  Instagram Professional account." Meta first tries "a few more times", alerts after 15 minutes,
  then unsubscribes. A receiver switched off overnight loses its Messenger subscription.
  **Re-run `meta-mcp setup --webhook-url`** after any outage longer than an hour.
- **Retries.** The generic Graph page says retries run "over the next 36 hours" and unacknowledged
  responses are dropped after 36 hours; the WhatsApp docs say Meta "retries delivery with decreasing
  frequency until the request succeeds, for up to 7 days". Plan for the shorter one.
- **See failures.** The dashboard shows them: **Messenger > Settings > Webhooks card > Show recent
  errors**.

### 6.6 Dedupe, ordering and signatures

- **Duplicates happen.** Retries "can result in duplicate webhook notifications", and Meta says "Your
  server should handle deduplication". Messenger: dedupe by `mid`. WhatsApp: by the `wamid`, plus the
  status for status events. meta-mcp does this with `INSERT OR IGNORE` on the event ID (Messenger
  `messenger:<mid>`, WhatsApp `whatsapp:<wamid>` and `whatsapp:status:<wamid>:<status>`).
- **Order is not guaranteed.** After failures messages "may not be delivered in the order they were
  sent", so order by `timestamp` (the `messages` event page says "Messages are always sent in
  order"). `inbox_list` sorts by event time, newest first. Meta also says batches of up to 1000
  updates are not guaranteed, so each POST is handled on its own, and each `entry` and `changes`
  array can hold several items.
- **Signature over raw bytes.** meta-mcp checks `X-Hub-Signature-256` over the exact bytes received.
  Meta documents that its Messenger signature uses "an escaped unicode version of the payload, with
  lowercase hex digits" (`äöå` is hashed as `\u00e4\u00f6\u00e5`) and that the WhatsApp pages do not
  repeat the note: **UNVERIFIED for WhatsApp.** If valid events that contain non-ASCII text fail
  with 401 `invalid signature` in the logs, report it. The key is the **App Secret**, never the verify
  token.

### 6.7 The inbox tools

Everything the receiver stores is readable through three tools.

1. `inbox_wait` blocks for up to 55 seconds and returns only events that arrive during the wait.
2. `inbox_list` lists stored events, newest first, with filters (`platform`, `kinds`, `since`,
   `unprocessed_only`, `account_id`).
3. `inbox_mark_processed` flags handled events so that `unprocessed_only` skips them. It is how an
   assistant keeps its place.

Event kinds: `message` (inbound text and media), `echo` (the Page's own sends, including replies
typed in the Page Inbox), `delivery` and `read` (Messenger receipts), `reaction`, `postback`,
`referral`, `optin`, `status` (WhatsApp sent, delivered, read, failed), `standby` (another app owns
the thread) and `other` (anything meta-mcp could not classify, with the raw payload kept). When no
webhook is configured and the inbox is empty, `inbox_list` says so in a `note`. The dashboard's
**Test** link sends placeholder payloads that are stored like real events: mark them processed.

### 6.8 One process: `http --with-webhook`

```bash
export META_MCP_API_KEYS="$(openssl rand -hex 32)"
export META_WEBHOOK_PUBLIC_URL=https://meta.example.org/webhook
meta-mcp http --with-webhook
```

One listener (`127.0.0.1:8788`) serves `/mcp` behind the API key, `/webhook` with its own signature
gate and no API key, and an open `/health`. The Host guard covers every route, and the host of
`META_WEBHOOK_PUBLIC_URL` is added to its allowlist automatically; without a public URL, add the
public host to `META_MCP_ALLOWED_HOSTS` or Meta gets 403. The app secret and the verify token are
required. The MCP server and the receiver share one inbox in one process. The alternative is two
processes (`meta-mcp webhook` and an MCP server) pointing at the same `META_MCP_INBOX_DB`.

---


## Part 7: WhatsApp

WhatsApp is the second track. Sending goes through the Graph API; **receiving only works through the
webhook** (Part 6), so set that up as well.

### 7.1 Create the WhatsApp use case (or add the product)

1. Prerequisites: a Facebook or managed Meta account, a developer registration (Part 1.1) and a
   WhatsApp-enabled phone to receive test messages.
2. In the App Dashboard (`developers.facebook.com/apps`) click **Create App**, enter a name and
   email, choose the use case **"Connect with customers through WhatsApp"**, pick or create a
   business portfolio, and click **Create app**. You land on **Customize use case > Connect on
   WhatsApp > Quickstart**. (The older flow was to add the WhatsApp product to a Business-type app.)
   Whether this use case can share an app with the Messenger one is UNVERIFIED (Part 1.3).
3. Open **Quickstart > Start using the API > API Setup** and connect the app to a Messaging account
   (an existing one, or **Create a WhatsApp Business account**). Meta creates a test WhatsApp
   account, a test Messaging account and a test business phone number. Test resources have "relaxed
   messaging limits and don't require a payment method" for templates. **Write down the phone number
   ID and the Messaging account (WABA) ID shown there.**
4. Click **Generate access token** (a temporary user token), choose the test number as **From**, add
   a **To** number (**To > Manage phone number list**; WhatsApp sends a confirmation code to that
   phone) and click **Send message**. That sends the `hello_world` template. Meta's 2022 blog says the
   temporary token "expires in less than 24 hours"; the current docs say user tokens "expire quickly,
   so you have to keep generating a new one every few hours".
5. Reply from the phone to open the 24-hour window, then send a free-form text with the System User
   token of 7.3.

**The new account model.** Meta is splitting the WABA into a WhatsApp account (one phone number) and
a Messaging account (templates, billing, webhook subscriptions). The Messaging account keeps the old
WABA ID, and phone number IDs, WABA IDs, endpoints and tokens keep working. meta-mcp treats `waba_id`
as an opaque Messaging account ID, and the send tools accept an optional `messaging_account_id`.

### 7.2 The test number and its recipients

Meta's archived Get Started pages say the test number can "send free messages to up to 5 recipient
phone numbers" ("up to 5 in total"). That is **SECONDARY**: the current docs say only that test
numbers have "relaxed messaging limits". A send to a number that is not on the list fails; meta-mcp's
hint for error 131030 ("recipient not in the allowed list") rests on community reports only, so
treat the code as **UNVERIFIED**. The test: add a recipient, send `hello_world`, then try an unlisted
number.

### 7.3 The System User token, step by step

A temporary token is fine for the first message. For anything durable use a System User token
(Meta's default for a developer on their own business data).

1. Open Business settings (`business.facebook.com/latest/settings`) > **System users** > **Add**.
   Choose **Admin** (full access to all the WhatsApp and Messaging accounts of the portfolio by
   default) or **Employee** (needs explicit grants).
2. Select the system user > **Assign assets**. Choose your app and enable **Manage app** ("Full
   control"). Choose your WhatsApp account with **Manage WhatsApp Business accounts** ("Full control").
   An Employee needs access on both the WhatsApp account and the Messaging account; access on the
   Messaging account alone is not sufficient, though the API currently cascades a grant from the
   Messaging account to the WhatsApp account. Without access you get error 200. Assignments can be
   slow to appear.
3. Click **Generate token**. Select the app, choose an expiration preference, tick
   `business_management`, `whatsapp_business_management` and `whatsapp_business_messaging`, and copy
   the token once. Store it as a secret. Tokens are opaque strings of variable length: do not parse
   them.
4. Give meta-mcp the token:

   ```bash
   meta-mcp setup --app-id 123456789012345 --app-secret-env META_APP_SECRET \
     --whatsapp --whatsapp-token "$WA_TOKEN" --waba-id 102290129340398 \
     --phone-number-id 106540352242922 --no-messenger --non-interactive
   ```

   With a user token that holds `business_management` and `whatsapp_business_management` the wizard
   can discover the WABA and numbers itself (`/me/businesses`, then each business's owned and client
   WhatsApp Business Accounts, then their phone numbers).

**Why a System User:** a user token "expires quickly". `whatsapp_business_messaging` sends messages
and receives message and status webhooks (and calls); `whatsapp_business_management` covers account
metadata, templates, phone numbers, analytics and all other webhooks; `business_management` is
"rarely needed" but is in Meta's documented recipe. A developer using only their own business data
needs no App Review or Advanced Access for these. Check what a token holds with `GET /debug_token`.

**Expiration.** Meta's page offers non-expiring tokens and 60-day expiring tokens, and says "Expiring
tokens are a security best practice", "Some businesses must use expiring tokens" and "All integrations
should adopt expiring tokens". Whether the "never" option is offered to your account is UNVERIFIED. Refresh
an expiring token **before day 60** with `meta-mcp token refresh --whatsapp`, which calls
`GET /oauth/access_token` with `grant_type=fb_exchange_token`, `client_id`, `client_secret`,
`set_token_expires_in_60_days=true` and `fb_exchange_token=<current token>`. Meta warns: "Failing to
do so would result in forfeiting the access token."

### 7.4 Register a real number

- **Eligible numbers.** You own it, it has a country and area code, and it can receive an SMS or a
  voice call. "Numbers already in use with WhatsApp cannot be registered unless they are deleted
  first." Registered numbers still work for calls and SMS but "cannot be used with WhatsApp
  Messenger". Mobile numbers are recommended; VoIP shows "Not Recommended" for SMS codes and
  "Standard" for voice codes.
- **Steps.** In **API Setup** add a phone number: this "generates a WhatsApp account containing that
  number, along with a Messaging account for your templates and billing". Then verify ownership and
  register. meta-mcp has no tool for these three calls; they are Meta's documented API:

  ```bash
  curl -X POST "https://graph.facebook.com/v26.0/<PHONE_NUMBER_ID>/request_code?code_method=SMS&language=en_US" \
    -H "Authorization: Bearer <TOKEN>"
  curl -X POST "https://graph.facebook.com/v26.0/<PHONE_NUMBER_ID>/verify_code" \
    -H "Authorization: Bearer <TOKEN>" -F code=<CODE_YOU_RECEIVED>
  curl -X POST "https://graph.facebook.com/v26.0/<PHONE_NUMBER_ID>/register" \
    -H "Authorization: Bearer <TOKEN>" -H "Content-Type: application/json" \
    -d '{"messaging_product":"whatsapp","pin":"<SIX_DIGIT_PIN>"}'
  ```

  `request_code` returns HTTP 400 with error 136024 if the number is already verified. The six-digit
  PIN becomes the two-step PIN; Meta is deprecating it for eligible numbers but still says to send a
  valid six-digit `pin` on `/register`. Registration allows 10 attempts per 72 hours (error 133016).
- **Caps.** New portfolios start at 2 registered numbers; the cap rises to 20 when the business is
  verified or reaches the 2,000 messaging limit.
- **Payment method.** Add one in the Billing Hub (`business.facebook.com/latest/billing_hub/payment_methods/`)
  before relying on service messages beyond the free tier (Part 7.12).
- **Display name.** Required at registration. Verification runs automatically "when you reach a
  higher messaging limit" (`name_status` becomes `APPROVED` or `DECLINED`). Changes are limited to
  10 per 30 days, and after an approved change you re-register within 14 days. The naming rules are in
  a Help Center article that could not be fetched: UNVERIFIED.
- **Error 133010** (number not registered) means you skipped `/register`.

### 7.5 The WhatsApp webhook

1. In the App Dashboard open **WhatsApp > Configuration** (apps created before December 2025), or
   **Use cases > Customize > Configuration** (apps created with the WhatsApp use case).
2. Enter the **Callback URL** and **Verify token** and click **Verify and save**. A list of fields you
   can subscribe to appears. Subscribe to **`messages`** at minimum (inbound messages and statuses).
3. Subscribe the app to the Messaging account: `POST /{waba-id}/subscribed_apps` (empty body, with the
   WhatsApp token). `meta-mcp setup --webhook-url` does this. Meta: "You must individually subscribe
   to every Messaging account for which you want to receive webhooks."
4. Optional: other fields (`account_update`, `account_alerts`, `phone_number_quality_update`,
   `phone_number_name_update`, `message_template_status_update`, `message_template_quality_update`,
   `business_capability_update`, `user_preferences`) need `whatsapp_business_management`. meta-mcp
   stores fields it does not recognise as `other` events.

**Why the dashboard:** `meta-mcp setup` first tries `POST /{app-id}/subscriptions` with
`object=whatsapp_business_account`, which Meta's WhatsApp docs describe but whose Graph reference says
is dashboard-only. When it fails the wizard prints these steps.

**Overrides.** Precedence is: phone number override, then Messaging account override, then the app's
callback URL. `meta-mcp setup --webhook-url URL --webhook-override` registers URL as a Messaging
account override (`override_callback_uri` and `verify_token` in the `subscribed_apps` body). A
per-number override uses `POST /{phone-number-id}` with `webhook_configuration`. Overrides cover
`messages`, `message_echoes`, `calls`, `consumer_profile`, `messaging_handovers`, `standby`, the
`group_*` fields and a few others; template and account-level webhooks always go to the app's
default URL.

**Testing.** The **Test** link next to the `messages` field "sends a test message to your test
webhook app", and message test webhooks work in both Development and Live mode. Meta also says: "Make
sure your app is in Live mode; some webhooks will not be sent if your app is in Dev mode." Which
webhooks are withheld in Development mode is UNVERIFIED.

### 7.6 Coexistence and personal numbers

- **A personal WhatsApp number cannot be registered as it is.** You must delete the personal account
  first. For a WhatsApp Business app number, either delete the account or onboard through a partner
  that supports business-app number onboarding. Deleting loses message history unless the partner
  route is used, and the number cannot be used with the Business app again unless it is deregistered.
- **Coexistence** (one number in the Business app and the Cloud API at once) is onboarded through
  Embedded Signup by "a Solution Partner or Tech Provider", and Meta's pages describe no direct
  developer path. Whether none exists is UNVERIFIED. Constraints Meta documents: Business app version
  2.24.17 or higher, a fixed 20 messages per second, one-time sync of contacts and up to 180 days of
  one-to-one history within 24 hours of onboarding, no groups, and messages sent from the Business
  app arrive as `smb_message_echoes` webhooks. Country support lists in archived pages differ and the
  current page lists none: check eligibility with your provider.

### 7.7 Messaging limits and throughput

| Limit | Value |
|---|---|
| Unique users messaged outside the window per rolling 24 hours (per business portfolio) | 250, then 2,000, 10,000, 100,000, unlimited. Replies inside a window do not count |
| Reaching 2,000 | Verify the business, or send 2,000 delivered template messages to unique users in 30 days with high quality |
| Automatic scaling beyond 2,000 | High quality and use of at least half the current limit in 7 days |
| Read your limit | `GET /{phone-number-id}?fields=whatsapp_business_manager_messaging_limit` (for example `TIER_250`); `messaging_limit_tier` is deprecated |
| Throughput per number | 80 messages per second by default, up to 1,000 on automatic upgrade (inbound and outbound count); 20 for coexistence numbers. Error 130429 |
| Pair rate limit | 1 message per 6 seconds to the same person (about 10 per minute, 600 per hour); bursts of up to 45 in 6 seconds borrow from future quota; retry after 4^X seconds. Error 131056 |
| Management endpoints | 200 requests per hour per app per Messaging account; 5,000 for active accounts with a registered number (errors 4, 80007) |
| Templates per Messaging account | 250 while the portfolio is unverified, 6,000 when verified with an approved display name; 100 creations per hour |

The old 1K tier no longer exists.

### 7.8 Templates and the 24-hour window

**The window.** "When a WhatsApp user messages you or calls you, a 24-hour timer called a customer
service window starts. If the user messages or calls you again before the timer expires, the timer
resets to 24 hours." While it is open you may send any free-form type. "When the window closes, you
can only send pre-approved template messages." The window is tracked per business and user, and only
a user message or call starts it. A person who messages from a Click-to-WhatsApp ad opens a window
whose replies are free for up to 7 days.

**Outside it**, a free-form send fails with 131047: "More than 24 hours have passed since the
recipient last replied to the sender number." Meta says errors arrive "either synchronously as a
Graph API response, asynchronously via Webhook, or sometimes through both", so handle both: a
synchronous 400 on the send call, and a `failed` status event in the inbox. meta-mcp helps in two
ways: sends check the local inbox and add a `window_warning` when the window looks closed (it never
blocks the send), and the error hint says to send a template.

**Templates.** `whatsapp_send_template` takes the template `name`, a `language` code (default
`en_US`) and the `components` list. Templates must be `APPROVED` in WhatsApp Manager; names are lowercase
letters, digits and underscores; parameters are positional (`{{1}}`) or named (`{{first_name}}`), chosen
when the template is created. The stock template on a fresh test number is `hello_world` in `en_US`.
Creating and approving templates happens in WhatsApp Manager and is outside meta-mcp. Opt-in is
mandatory before you message anyone, and conversations may be started only with an approved template
(Part 0.4). Errors: 132001 (missing or not approved), 132000 (parameter count), 132012 (parameter
format), 132015 (paused), 132016 (disabled).

### 7.9 Sending media

Use `whatsapp_send_media` with `path` (a file in `META_MCP_FILES_DIR`), `media_id` (from
`whatsapp_upload_media`) or `url`. Prefer uploads: Meta calls `link` "not recommended" and caches the
file for 10 minutes. Uploaded media persists 30 days; media IDs received in webhooks expire after 7
days. meta-mcp checks the type and size limit before any request and always sends the MIME type
explicitly, because a mismatch (error 131053) is common. The limits are in
[TOOLS.md](TOOLS.md#whatsapp_send_media). Documents outside Meta's list "may be sent via the API,
but they are not supported and may not be handled as expected".

### 7.10 Receiving through the inbox

Inbound messages arrive as `message` events: text, image, audio (with a voice flag), video, document
(with `filename`), sticker, location, contacts, interactive replies (`button_reply`, `list_reply`),
template quick-reply `button` taps, reactions, orders, `unsupported` types (polls, for instance, arrive
as `unsupported` with error 131051 or 131060), edits (within 15 minutes) and revokes (within two days).
Media events carry the media `id`, MIME type and a base64 `sha256`. Each message you send produces up to
three `status` events (`sent`, `delivered`, `read`), or `failed` with error details.

To fetch media use `whatsapp_download_media` with the event attachment's `media_id`: it asks Graph for
a fresh URL, which expires after 5 minutes, and downloads it with your token, which Meta requires. A
404 means the URL expired. `whatsapp_mark_read` sets the blue ticks, and with `typing` shows a typing
indicator for up to 25 seconds, so use it only when a reply follows.

### 7.11 Business-scoped user IDs

Since April 2026 webhooks carry a business-scoped user ID (BSUID): `contacts[].user_id` and
`messages[].from_user_id`, plus `contacts[].profile.username` when the person has a username. If a
person adopts a username, `wa_id` and `from` are **omitted** unless you messaged or called them within
30 days, received a message or call from them within 30 days (per business phone number), or they are
in your contact book. Status events add `recipient_user_id`. A person who changes phone number gets a
new BSUID and a `system` message (`user_changed_number` or `user_changed_user_id`).

meta-mcp copes with both identities. Inbound events use `from`, else `from_user_id`, as the `sender`,
and keep `_sender_user_id`, `_sender_name` and `_sender_username` in `raw`. Sends accept either a phone
number or a BSUID such as `US.13491208655302741918` in `to`. Meta accepts the `recipient` field for
BSUIDs scoped to the same business portfolio as the sending number (when both `to` and `recipient`
are present, `to` wins), for every message type except one-tap, zero-tap and copy-code authentication
templates, which need a phone number (error 131062).

### 7.12 Pricing as of 2026-10-01

Per-message pricing has applied since 2025-07-01. Meta publishes USD. The table shows the North America
rate card effective 2026-10-01 (Canada and the United States share one "North America" rate) with the
CAD figure in parentheses, converted at the Bank of Canada daily rate of 1.3993 and rounded. Rates
depend on the category and on the recipient's country calling code, so use Meta's pricing page for
other markets.

| Category | Delivered message | USD | CAD |
|---|---|---|---|
| Marketing (template) | per message | 0.0250 | (0.0350) |
| Utility (template, and utility inside an open window from 2026-10-01) | per message | 0.0034 | (0.00476) |
| Authentication (template) | per message | 0.0034 | (0.00476) |
| Service (free-form text inside an open 24-hour window) | per message beyond 1,000 free per number per month | 0.0034 | (0.00476) |
| 1,000 service messages beyond the free tier | | 3.40 | (4.76) |

User-to-business messages are free and a message is charged only when delivered. Service messages were
free until 2026-09-30; from 2026-10-01 the first 1,000 per business phone number per month are free
(the counter resets at midnight in the Messaging account's time zone, with no rollover). **A payment
method is still required**: without one Meta delivers service messages inside the free tier and stops
delivering after it. Test accounts need none for templates. Meta bills in one of 16 currencies (USD,
AED, ARS, AUD, BRL, CLP, COP, EUR, GBP, IDR, INR, MXN, MYR, PEN, SAR, SGD): CAD is not offered, and the
currency cannot be changed after the account is created. Meta may change prices only on January 1,
April 1, July 1 and October 1.

---


## Part 8: Going to production

### 8.1 HTTP mode with API keys, behind TLS

stdio is right when the client and the server share a machine and a user. For anything else run HTTP
mode behind TLS. The proxy, systemd and backup examples in this part are illustrations, not Meta facts:
adapt them to your platform and check its documentation.

```bash
export META_MCP_API_KEYS="$(openssl rand -hex 32)"    # comma-separate one key per client
export META_MCP_ALLOWED_HOSTS=meta.example.org
meta-mcp http --with-webhook
```

- **Keys.** At least one, each at least 32 characters, or the server refuses to start. Clients send
  `Authorization: Bearer <key>` or `X-API-Key`. A wrong key gets 401.
- **Bind.** `127.0.0.1:8788` (`META_MCP_HOST`, `META_MCP_PORT`). Binding elsewhere logs a warning. The
  process speaks plain HTTP: terminate TLS in a reverse proxy (8.2) or a tunnel.
- **Host allowlist.** The `Host` header must be loopback, the bind host, an entry of
  `META_MCP_ALLOWED_HOSTS` (`host` or `host:port`), or the host of `META_WEBHOOK_PUBLIC_URL` when
  `--with-webhook` is set. An `Origin` header, when present, must be allowed too. Otherwise the answer
  is 403 `forbidden host or origin: add the host to META_MCP_ALLOWED_HOSTS`. If the proxy rewrites the
  `Host` header, list the rewritten value.
- **No CORS, stateless sessions, 2 MiB request limit.** `/health` is open and answers `{"ok":true}`.
- **Clients.** Part 4.8 has the Claude Code, Cursor and generic snippets for a remote server.

### 8.2 A TLS reverse proxy (nginx example)

```nginx
server {
    listen 443 ssl;
    server_name meta.example.org;
    # ssl_certificate and ssl_certificate_key: your certificate and key

    location /mcp {
        proxy_pass http://127.0.0.1:8788;
        proxy_set_header Host $host;
    }
    location /webhook {
        proxy_pass http://127.0.0.1:8788;
        proxy_set_header Host $host;
    }
}
```

Adapt it to your proxy. The two requirements are a valid certificate (Meta refuses self-signed ones)
and an unchanged `Host` that matches `META_MCP_ALLOWED_HOSTS`. Do not expose `/health` of a
standalone `meta-mcp webhook` publicly: it reports event counts.

### 8.3 Read-only mode, the file jail and log levels

- **Read-only.** `META_MCP_READ_ONLY=true` removes the 11 tools that send or change state (list in
  [TOOLS.md](TOOLS.md#read-only-mode)). Start here and open up deliberately.
- **File jail.** `META_MCP_FILES_DIR` is the only place file tools read and write. Use an empty,
  dedicated directory, never the one that holds `credentials.json`, and read the limits in
  [SECURITY.md](../SECURITY.md#file-jail). `META_MCP_MAX_FILE_BYTES` caps file size (default 100 MiB).
- **Logs.** They go to stderr (journald under systemd). `META_MCP_LOG` takes tracing filter syntax;
  the default is `meta_mcp=info`. Use `meta_mcp=debug` to see retries and `meta_mcp=trace` for the most
  detail. Tokens and the app secret are never logged, webhook bodies are not logged (batch counts
  are), and error text is scrubbed of anything shaped like a Graph token.

### 8.4 A systemd unit

An example for `http --with-webhook`. Adapt paths and the user to your system.

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin meta-mcp
sudo install -d -m 0700 -o meta-mcp -g meta-mcp /var/lib/meta-mcp
sudo install -m 0755 ~/.cargo/bin/meta-mcp /usr/local/bin/meta-mcp
sudo install -d -m 0750 /etc/meta-mcp
```

`/etc/meta-mcp/meta-mcp.env` (mode 0600, read by systemd as root). The service needs the app secret
and the verify token to start its webhook; the Page and WhatsApp tokens come from `credentials.json`
once the wizard has written it:

```
META_APP_SECRET=replace-with-app-secret
META_WEBHOOK_VERIFY_TOKEN=replace-with-64-hex-characters
META_MCP_API_KEYS=replace-with-64-hex-characters
META_MCP_ALLOWED_HOSTS=meta.example.org
META_WEBHOOK_PUBLIC_URL=https://meta.example.org/webhook
META_MCP_FILES_DIR=/var/lib/meta-mcp/files
```

`/etc/systemd/system/meta-mcp.service`:

```ini
[Unit]
Description=meta-mcp (MCP over HTTP plus the Meta webhook receiver)
After=network-online.target
Wants=network-online.target

[Service]
User=meta-mcp
Group=meta-mcp
EnvironmentFile=/etc/meta-mcp/meta-mcp.env
Environment=META_MCP_HOME=/var/lib/meta-mcp
Environment=META_MCP_INBOX_DB=/var/lib/meta-mcp/inbox.db
StateDirectory=meta-mcp
StateDirectoryMode=0700
ExecStartPre=/usr/bin/mkdir -p /var/lib/meta-mcp/files
ExecStart=/usr/local/bin/meta-mcp http --with-webhook
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
CapabilityBoundingSet=
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
```

Start the service first, so the receiver can answer Meta's handshake, then sign in and subscribe as the
service user. On a server with no browser use paste mode and open the printed URL on any machine. The
wizard prompts for the App ID and App Secret (hidden); `$VERIFY_TOKEN` is the value in the env file.

```bash
sudo systemctl enable --now meta-mcp
sudo -u meta-mcp env META_MCP_HOME=/var/lib/meta-mcp meta-mcp setup --redirect-mode paste --no-browser \
  --webhook-url https://meta.example.org/webhook --verify-token "$VERIFY_TOKEN"
sudo systemctl restart meta-mcp      # the server reads credentials.json at startup
sudo -u meta-mcp env META_MCP_HOME=/var/lib/meta-mcp meta-mcp doctor --webhook
```

### 8.5 The token refresh schedule

| Token | Lifetime | What to schedule |
|---|---|---|
| User token | About 60 days | `meta-mcp token refresh`, well inside 60 days. The doctor warns under 10 days left |
| Page tokens derived from a long-lived user token | No expiry, but can be invalidated | `token refresh` re-derives them; run `meta-mcp doctor` daily (Meta says to check validity "at least daily" without its SDKs) and treat exit code 1 as an alert |
| WhatsApp System User token, expiring kind | 60 days | `meta-mcp token refresh --whatsapp` before day 60 |

The server reads `credentials.json` at startup, so restart it after a refresh. A monthly systemd
timer (`OnCalendar=monthly`) or cron job running `meta-mcp token refresh` as the service user, followed
by `systemctl restart meta-mcp`, leaves a wide margin inside the 60 days; add `meta-mcp token refresh
--whatsapp` when you use an expiring WhatsApp token.

### 8.6 Re-subscribing after downtime

A Messenger subscription is dropped after 1 hour of failed deliveries (Part 6.5). After any outage
longer than that, run `meta-mcp setup --webhook-url https://meta.example.org/webhook` again (it signs
in again unless you pass a token flag, and the subscription steps are idempotent) and
`meta-mcp doctor --webhook` to confirm. Meta cannot replay what you missed, and a
Messenger subscription that was dropped delivers nothing until you subscribe again. For WhatsApp Meta
keeps retrying for up to 7 days (36 hours on the generic Graph page), so a receiver that comes back
catches up inside that window.

### 8.7 Rotating the app secret

1. In the App Dashboard open **Settings > Basic** and reset the App Secret. Only an Administrator can.
   Meta: "It is not possible to programmatically rotate the app secret."
2. Update every place that holds it: `credentials.json` (rerun `meta-mcp setup` with the new secret in
   the environment, or edit `app_secret`) and `META_APP_SECRET` in your environment files, for the
   webhook process and the MCP server alike.
3. Restart. Until you do, webhook signatures fail with 401 and, with **Require App Secret** on, calls
   fail for lack of a valid `appsecret_proof`.
4. Run `meta-mcp doctor --webhook`, then `meta-mcp setup --webhook-url ...` if deliveries stopped.

Whether existing tokens survive a reset is not stated in the fetched pages: UNVERIFIED. If a token
fails with 190 afterwards, sign in again.

### 8.8 Backups

- **`credentials.json`** holds the app secret and every token. Back it up only encrypted, with the
  same care as the original, or skip the backup and run `meta-mcp setup` again.
- **`inbox.db`** is the only copy of your webhook history, and Meta cannot replay it. It is a SQLite
  database in WAL mode: either stop the service and copy `inbox.db` together with `inbox.db-wal`, or use
  SQLite's online backup, `sqlite3 /var/lib/meta-mcp/inbox.db ".backup '/backups/inbox.db'"`. It holds
  customer messages and meta-mcp never purges it: apply your own retention.
- **The files directory** is whatever you put in it.

### 8.9 Standing chores

- Refresh tokens (8.5) and watch `meta-mcp doctor`.
- Complete the **Data Use Checkup** each year once the app is published (Part 2.8).
- Keep the app active: a login, an API call or a webhook inside every 90 days.
- Watch the Graph version. v26.0 is the default (`META_GRAPH_VERSION`). "Each version is guaranteed to
  operate for at least two years" and a version stops being usable two years after its successor is
  released (v20.0 was removed on 2026-09-24, and v21.0 goes on 2027-01-21). Upgrade deliberately, not
  by accident.

---


## Part 9: Troubleshooting

Read the error text first: it has the Graph code, the subcode, Meta's message, a `fbtrace` ID (what
Meta support asks for) and, for known codes, a hint.

### 9.1 Graph errors by code and subcode

Messenger and general Graph codes (Meta's error tables as documented on 2026-10-24):

| Code / subcode | Cause | Fix |
|---|---|---|
| 190 (subcodes 458 app not installed, 459 user checkpointed, 460 password changed or logged out, 463 expired, 464 unconfirmed user, 467 invalid) | The token is dead | `meta-mcp token refresh` while the user token is valid, else `meta-mcp setup`. A Page token from a short-lived user token lasts only 1 hour |
| 10, 200 to 299, 230 | Permission or Page task missing, or app role missing | Check scopes with `meta-mcp token show` or `debug_token`; the person needs the `MESSAGING` and `MODERATE` tasks (Part 2.1); in Development mode only role users can be reached |
| 200 / 2018028 | "Cannot message users who are not admins, developers or testers of the app until pages_messaging permission is reviewed and the app is live." | Give the person a Tester role, or run the gate test (Part 2.5) |
| 10 / 2018278, 10 / 2534022 | "This message is sent outside of allowed window." | The person must message the Page again. There is no tag for AI replies (Part 5.2). Read the **subcode**: meta-mcp's built-in window hint is keyed to code 100 with subcode 2018278, so with code 10 the hint shown may be the generic permission one |
| 10 / 2018300, 100 / 2534037 | Another app owns the thread (a default app or Meta Business AI) | Wait for `pass_thread_control` (Part 5.5) |
| 100 / 2018001 | "No matching user found": the ID is not a PSID of this Page (for example an app-scoped Facebook Login ID, or a PSID of another Page) | Use `counterpart.psid` from this Page's conversations |
| 551, 200 / 1545041 | "This person isn't available right now." | The person blocked the Page, deactivated, or is unreachable |
| 2 | May mean the customer blocked the business | None; also a temporary error |
| 100 | Invalid parameter, a bad ID, or a retired tag | Check the recipient and body; `CONFIRMED_EVENT_UPDATE`, `ACCOUNT_UPDATE` and `POST_PURCHASE_UPDATE` return it since 2026-04-27 |
| 100 / 2018008 | Meta could not fetch the URL | The URL needs a valid TLS certificate, a valid size and a fast server, or upload with `path` |
| 100 / 2018294 | Video upload timed out or is corrupted | Retry, or re-encode |
| 100 / 2018047 | "Upload attachment failure", usually the type does not match the file | Match `kind` to the file |
| 100 / 2018074 | Possible invalid attachment ID, or not yours | Re-upload (IDs expire after 90 days and belong to one Page) |
| 100 / 2018109 | Attachment size exceeds the limit | 25 MB, and 8 MB for an image fetched by URL |
| 100 / 2018014 | "Cannot send both message and state at the same time" | Send a sender action as its own request |
| 100, "nonexisting field" | The conversation fields beyond the documented ones are not available | meta-mcp retries once with the documented fields and sets `fields_reduced` |
| 100, "After Cursor ... exceeds the max limit" | Paged too deep | Stop paging |
| 9000001 | "This Message has been deleted by the user or the business" | Older than the 20 most recent messages (Part 5.3) |
| 2018247 | "Insufficient permission to access user profile." | Business Asset User Profile Access at Advanced Access (Part 5.6) |
| 2018218 | "No profile available for this user." | The account was created from a phone number |
| 4, 17, 32, 341, 613, 80001, 80006 | Rate limits (app, user, Pages, application, custom, Pages with a Page token, Messenger) | Wait; Meta says stop calling when a limit is reached. 613/2534040 is "Calls to this api have exceeded the rate limit" |
| 1, 2 | Temporary Graph trouble | Retry |
| 368 | Temporarily blocked for policy | Check the Page's policy notices; the hint says the action was blocked as abusive or policy violating |

WhatsApp codes (Meta's current error table; HTTP statuses are not in the current table):

| Code | Cause | Fix |
|---|---|---|
| 0, 190 | Token expired, invalidated or revoked | `meta-mcp token refresh --whatsapp`, or a new token (Part 7.3) |
| 3, 10, 200 to 299, 131005 | Permission missing, or the system user lacks asset access | Check scopes with `debug_token`; assign the assets (Part 7.3) |
| 100 | Unsupported or misspelled parameter, or an over-long value | Fix the request |
| 131008, 131009 | Missing or invalid parameter value | 131009 also covers a bad `message_id` on read receipts and a reaction target older than 30 days |
| 131047 | More than 24 hours since the person last replied | Send a template (Part 7.8) |
| 131026 | The recipient is not on WhatsApp, has not accepted the new terms, or uses an old version | Ask the person to confirm, accept and update |
| 131030 | Recipient not in the allowed list of a test number (community reports; **UNVERIFIED**) | Add the number to the recipient list (Part 7.2) |
| 131021 | Sender and recipient are the same number | Use another recipient |
| 131042 | Payment method problem | Check the Billing Hub, currency, time zone and credit line |
| 131049 | "Not delivered to maintain healthy ecosystem engagement" | Wait at least 24 hours before resending a template |
| 131048 | Restricted after too many messages were blocked or flagged as spam | Check the number's quality status |
| 131050 | The person opted out of marketing messages | Stop |
| 130403 | The business blocked the person | Do not retry; unblock to resume |
| 130497 | The account is restricted from messaging some countries | A policy issue |
| 131051 | Unsupported message type | Check `type` |
| 131052, 131053 | Inbound media too large to download (over 100 MB); outbound media upload failed (usually a MIME or extension mismatch) | Ask the person to resend; match the MIME type |
| 131056 | Pair rate limit (1 message per 6 seconds to the same person) | Wait; other recipients are unaffected |
| 130429, 80007, 4 | Throughput or rate limit | Slow down and retry |
| 131057 | Account in maintenance mode | Retry shortly |
| 131062 | A BSUID recipient is not supported for this message | Send to a phone number (Part 7.11) |
| 132000, 132001, 132012, 132015, 132016 | Template: parameter count mismatch, missing or not approved, parameter format, paused, disabled | Fix or replace the template |
| 133010 | The number is not registered | Call `/register` (Part 7.4) |
| 133016, 136024 | Register rate limit (10 per 72 hours); the number is already verified | Wait; skip `request_code` |
| 131000, 131016, 133004 | Server errors or downtime | Retry with backoff |

### 9.2 The wizard's failure messages

| Message | Cause | Fix |
|---|---|---|
| `App ID or App Secret rejected` | `GET /{app-id}` failed with 190 or 101 | Copy both again from **Settings > Basic**; check **Settings > Advanced > App Type** is not Native/Desktop (Part 1.7) |
| `no callback received: is http://localhost:<port>/callback listed under Facebook Login for Business > Settings > Valid OAuth Redirect URIs?` | The sign-in did not reach the listener within `--timeout-seconds` | Register the exact URI (Part 3.2); or use `--redirect-mode paste` |
| A busy port | Something else uses 8766 | Free it, or pass `--port N` and register that URI |
| A state mismatch (HTTP 400 on the callback) | The callback did not carry the `state` this run created | Rerun the wizard and use the new URL it prints |
| `error_reason=user_denied` | The sign-in was cancelled | Rerun and approve |
| `missing scope X: rerun with --rerequest` | The person declined a permission | Rerun with `--rerequest`. Fatal only when `pages_messaging` is missing in a Messenger setup |
| A per-Page warning with a Graph hint | The conversations probe failed, usually code 10, 200 or 230 | Page tasks and permissions (Parts 2.1, 1.3) |
| The reachability self-check fails | The receiver or the tunnel is down, or the URL is wrong | Start `meta-mcp webhook` and the tunnel, then rerun (Part 6.4) |
| The WhatsApp app-level subscription errors | Meta's Graph reference says WhatsApp objects are dashboard-only | Follow the dashboard steps the wizard prints (Part 7.5) |
| A usage error naming a flag, exit code 2 | `--non-interactive` and a missing value | Pass the flag or its environment variable |
| `--webhook-url` refused | Plain `http` to a non-loopback host | Use `https` |

### 9.3 The webhook is not firing

Work down the list; stop at the first failure.

1. `meta-mcp doctor --webhook` shows the callback answering our own verification. If not, the receiver
   or the tunnel is down.
2. The URL is public HTTPS with a valid certificate. "Self-signed certificates are not supported."
3. Meta's verification passed: the verify token you gave Meta equals `webhook.verify_token`. A mismatch
   gets 403 `verification failed` in the receiver's log. For WhatsApp, if you answer the handshake with
   anything else, "Meta will consider your webhook endpoint unverified, and webhooks will not be sent".
4. The signature passes: the receiver logs `webhook rejected: missing or invalid signature` when the
   app secret is wrong (for example after a reset).
5. Fields are subscribed at **both** levels (app and Page); `meta_status` shows the Page's
   `subscribed_fields`, `meta-mcp doctor` checks the app subscription.
6. The Page installed the app and has not disabled the App platform in its App Settings.
7. In Development mode, webhooks fire only for people with an app role; for WhatsApp some webhooks need
   Live mode.
8. The app has not been unsubscribed after an hour of failures: re-run `meta-mcp setup --webhook-url`
   (Part 6.5). The dashboard's **Messenger > Settings > Webhooks card > Show recent errors** lists them.
9. Behind `http --with-webhook`: the proxy's `Host` is allowed (Meta sees a 403 `forbidden host or
   origin`, and the log says `rejected request with disallowed Host or Origin header`), and `/webhook`
   is routed to the process.
10. Use the dashboard's **Test** link to separate server bugs from platform issues.

### 9.4 "URL Blocked" and "Insecure Login Blocked"

- **URL Blocked.** Meta's wording (from a seven-year-old community thread, so it may have drifted):
  "This redirect failed because the redirect URI is not whitelisted in the app's Client OAuth Settings.
  Make sure Client and Web OAuth Login are on and add all your app domains as Valid OAuth Redirect
  URIs." **Fix:** register the URI character for character, port and path included (Part 3.2), turn
  **Client OAuth Login** and **Web OAuth Login** on, and press **Check URI**.
- **Insecure Login Blocked: "You can't get an access token or login to this app from an insecure
  page."** A community report, not a Meta page. The redirect is plain `http` and HTTPS is enforced.
  **Fix:** rerun with `--redirect-mode paste` and register `https://localhost:8766/callback`, or use a
  tunnel with HTTPS, or paste a token (Part 3.5).
- A different `redirect_uri` at code exchange fails because it "must be the same as the original
  request_uri that you used when starting the OAuth login process". The error code for it is UNVERIFIED.
  meta-mcp sends the same value in both steps.

### 9.5 Token expired flows

| Symptom | What happened | Do this |
|---|---|---|
| `meta-mcp doctor` says fewer than 10 days left | The long-lived user token (about 60 days) is about to expire. Page tokens derived from it do not expire, but a valid user token is what lets you refresh and re-derive them | `meta-mcp token refresh`, then restart the server |
| `token refresh` fails with 190 | The user token already expired: "You can not use an expired token to request a long-lived token" | `meta-mcp setup` |
| Error 190 on Page calls | The Page token was invalidated: a password change or logout (subcode 460), or app inactivity | `meta-mcp setup` |
| Error 190 about an hour after setup with a pasted Page token | The token came from a short-lived user token and lasts 1 hour | Use the wizard's login, or a long-lived user token |
| WhatsApp error 0 or 190 | A user token (60 days) or an expiring System User token ended | `meta-mcp token refresh --whatsapp` if it is still valid, else generate a new token |
| Everything fails after you reset the app secret | The stored secret is stale | Part 8.7 |
| Every call fails after 90 idle days | The app was inactive and its tokens were invalidated | `meta-mcp setup` (Part 2.9) |

---


## Appendix A: Permission reference

All of these start at Standard Access (the default). Any of them moves to Advanced Access only
through App Review plus Business Verification. Dependencies: `pages_messaging` depends on
`pages_manage_metadata` and `pages_show_list`; `pages_read_engagement` and `pages_manage_metadata`
depend on `pages_show_list`. Meta's permission reference prints `page_manage_metadata` and
`page_utility_messaging` in places: the real names are `pages_manage_metadata` and
`pages_utility_messaging`.

| Permission or feature | What it unlocks (Meta's wording) | What meta-mcp uses it for |
|---|---|---|
| `pages_show_list` | "allows your app to access the list of Pages a person manages" | `GET /me/accounts` to find the Page ID and Page token; a prerequisite of subscribing |
| `pages_manage_metadata` | "allows your app to subscribe and receive webhooks about activity on the Page, and to update settings on the Page" | `POST /{page-id}/subscribed_apps`; also required by the Conversations and Message reads |
| `pages_messaging` | "allows your app to manage and access Page conversations and calling in Messenger" | The Send API, the Conversations API, the Page token for messaging |
| `pages_read_engagement` | "allows your app to read content (posts, photos, videos, events) posted by the Page, read followers data (including name, PSID), and profile picture, and read metadata and other insights about the Page" | Required by the Page Conversations read endpoint |
| `business_management` | "allows your app to read and write with the Business Manager API" | A documented dependency of `pages_messaging` and `pages_show_list`; needed when a System User is involved; listed in the WhatsApp token recipe |
| `public_profile` | Required and not removable in the Messenger use case | Nothing directly |
| `pages_read_user_content`, `pages_manage_engagement` | Read and delete user comments; create, edit and delete comments | Not needed for direct messages; not offered in the Messenger use case |
| `pages_utility_messaging` | Utility (template) messages outside the window | Not used |
| `pages_user_locale`, `pages_user_timezone`, `pages_user_gender` | The matching profile fields | Not used by the tools |
| `instagram_basic`, `instagram_manage_messages` | Instagram messaging through the linked Page | Requested only with `--instagram`; no Instagram tools |
| `whatsapp_business_messaging` | Sends messages and receives message and status webhooks (and calls) | All WhatsApp sends |
| `whatsapp_business_management` | Account metadata, templates, phone numbers, analytics and all other webhooks | WhatsApp discovery and the doctor |
| Feature: Business Asset User Profile Access | The user profile fields `id`, `name`, `first_name`, `last_name`, `profile_pic` ("Requires App Review"; "only available with business verification") | `messenger_get_profile` |
| Feature: Human Agent | The `HUMAN_AGENT` tag (7 days; App Review and business verification) | Accepted by `messenger_send_text`; not for automated messages |

A Tech Provider "access verification" (businesses building apps used by other businesses) does not
apply to a solo, own-Page app. Its list includes `business_management`, `pages_show_list`,
`pages_read_engagement` and `pages_utility_messaging`, but not `pages_messaging` or
`pages_manage_metadata`.

## Appendix B: Scopes requested by the wizard

| Setup | Scopes in the login dialog |
|---|---|
| Messenger (always) | `pages_show_list`, `pages_manage_metadata`, `pages_messaging`, `pages_read_engagement`, `business_management` |
| With `--whatsapp` | adds `whatsapp_business_messaging`, `whatsapp_business_management` |
| With `--instagram` | adds `instagram_basic`, `instagram_manage_messages` |
| With `--config-id` | the configuration decides; `scope` may be omitted |

`business_management` is requested because Meta documents it as a dependency of `pages_messaging`
and `pages_show_list`. A declined scope is reported as `missing scope X`; `--rerequest` asks again.

## Appendix C: Meta documentation links

Every URL below is a page the research read. A link ending in `.md` returns the raw Markdown of the page.

**Account, app and access**

[Register](https://developers.facebook.com/documentation/development/register.md), [Create an app](https://developers.facebook.com/documentation/development/create-an-app.md), [Messenger use case](https://developers.facebook.com/documentation/development/create-an-app/messenger-use-case.md), [Apps without a use case](https://developers.facebook.com/documentation/development/create-an-app/no-use-case.md), [Other app types](https://developers.facebook.com/documentation/development/create-an-app/other-app-types.md), [App dashboard](https://developers.facebook.com/documentation/development/create-an-app/app-dashboard.md), [Basic settings](https://developers.facebook.com/documentation/development/create-an-app/app-dashboard/basic-settings.md), [App states](https://developers.facebook.com/documentation/development/create-an-app/app-dashboard/app-states.md), [App types](https://developers.facebook.com/documentation/development/create-an-app/app-dashboard/app-types.md), [App modes](https://developers.facebook.com/documentation/development/build-and-test/app-modes.md), [App roles](https://developers.facebook.com/documentation/development/build-and-test/app-roles.md), [Test users](https://developers.facebook.com/documentation/development/build-and-test/test-users.md), [Permissions reference](https://developers.facebook.com/documentation/development/permissions.md), [Access levels](https://developers.facebook.com/documentation/development/graph-api/overview/access-levels.md), [Release (go live)](https://developers.facebook.com/documentation/development/release.md), [Access verification](https://developers.facebook.com/documentation/development/release/access-verification.md), [Business Verification](https://developers.facebook.com/documentation/development/release/business-verification.md), [Maintaining data access](https://developers.facebook.com/documentation/development/maintaining-data-access.md), [Human Agent feature](https://developers.facebook.com/documentation/development/features-reference/human-agent.md), [Business Asset User Profile Access](https://developers.facebook.com/documentation/development/features-reference/business-asset-user-profile-access.md), [App Review](https://developers.facebook.com/documentation/resp-plat-initiatives/individual-processes/app-review.md), [App Review submission guide](https://developers.facebook.com/documentation/resp-plat-initiatives/individual-processes/app-review/submission-guide.md), [Data Use Checkup](https://developers.facebook.com/documentation/resp-plat-initiatives/individual-processes/data-use-checkup.md), [Data Use Checkup FAQ](https://developers.facebook.com/documentation/resp-plat-initiatives/individual-processes/data-use-checkup/faq.md), [Pages API overview](https://developers.facebook.com/documentation/pages-api/overview.md).

**Facebook Login, tokens and system users**

[Manual login flow](https://developers.facebook.com/documentation/facebook-login/guides/advanced/manual-flow.md), [Login security (Strict Mode, App Secret proof)](https://developers.facebook.com/documentation/facebook-login/security.md), [Login for devices](https://developers.facebook.com/documentation/facebook-login/for-devices.md), [Facebook Login for Business](https://developers.facebook.com/documentation/facebook-login/facebook-login-for-business.md), [Access tokens](https://developers.facebook.com/documentation/facebook-login/guides/access-tokens.md), [Long-lived tokens](https://developers.facebook.com/documentation/facebook-login/guides/access-tokens/get-long-lived.md), [Debugging and error handling](https://developers.facebook.com/docs/facebook-login/access-tokens/debugging-and-error-handling), [Strict URI matching announcement](https://developers.facebook.com/blog/post/2017/12/18/strict-uri-matching/), [Temporary token blog post](https://developers.facebook.com/blog/post/2022/12/05/auth-tokens), [System users](https://developers.facebook.com/docs/business-management-apis/system-users), [Overview](https://developers.facebook.com/docs/business-management-apis/system-users/overview), [Install apps and generate tokens](https://developers.facebook.com/docs/business-management-apis/system-users/install-apps-and-generate-tokens), [Permissions](https://developers.facebook.com/docs/business-management-apis/system-users/guides/permissions), [API calls](https://developers.facebook.com/docs/business-management-apis/system-users/guides/api-calls).

**Messenger Platform**

[Overview](https://developers.facebook.com/documentation/business-messaging/messenger-platform/overview.md), [Get started](https://developers.facebook.com/documentation/business-messaging/messenger-platform/get-started.md), [Quick start](https://developers.facebook.com/documentation/business-messaging/messenger-platform/getting-started/quick-start.md), [Webhooks](https://developers.facebook.com/documentation/business-messaging/messenger-platform/webhooks.md), [Webhook event: messages](https://developers.facebook.com/documentation/business-messaging/messenger-platform/webhooks/webhook-events/messages.md), [Webhook event: message echoes](https://developers.facebook.com/documentation/business-messaging/messenger-platform/webhooks/webhook-events/message-echoes.md), [Webhook event: standby](https://developers.facebook.com/documentation/business-messaging/messenger-platform/webhooks/webhook-events/standby.md), [Conversations](https://developers.facebook.com/documentation/business-messaging/messenger-platform/conversations.md), [Conversation routing](https://developers.facebook.com/documentation/business-messaging/messenger-platform/conversation-routing.md), [Send messages](https://developers.facebook.com/documentation/business-messaging/messenger-platform/send-messages.md), [Saving assets (attachment upload)](https://developers.facebook.com/documentation/business-messaging/messenger-platform/send-messages/saving-assets), [Sender actions](https://developers.facebook.com/documentation/business-messaging/messenger-platform/send-messages/sender-actions), [Utility messages](https://developers.facebook.com/documentation/business-messaging/messenger-platform/send-messages/utility-messages.md), [Policy](https://developers.facebook.com/documentation/business-messaging/messenger-platform/policy.md), [App Review (Messenger)](https://developers.facebook.com/documentation/business-messaging/messenger-platform/app-review.md), [FAQ](https://developers.facebook.com/documentation/business-messaging/messenger-platform/faq.md), [Error codes](https://developers.facebook.com/documentation/business-messaging/messenger-platform/error-codes.md), [Changelog](https://developers.facebook.com/documentation/business-messaging/messenger-platform/changelog.md), [General best practices](https://developers.facebook.com/documentation/business-messaging/messenger-platform/introduction/general-best-practices.md), [Rate limiting](https://developers.facebook.com/documentation/business-messaging/messenger-platform/overview/rate-limiting.md), [User profile](https://developers.facebook.com/documentation/business-messaging/messenger-platform/identity/user-profile.md), [Send API reference (server-rendered copy)](https://developers.facebook.com/docs/messenger-platform/reference/send-api%23limits), [Attachment Upload API reference (server-rendered copy)](https://developers.facebook.com/docs/messenger-platform/reference/attachment-upload-api%23limits).

**Graph API reference and policies**

[Page conversations](https://developers.facebook.com/docs/graph-api/reference/page/conversations/), [Conversation](https://developers.facebook.com/docs/graph-api/reference/conversation/), [Message](https://developers.facebook.com/docs/graph-api/reference/message/), [Page messages](https://developers.facebook.com/docs/graph-api/reference/page/messages/), [Page subscribed apps](https://developers.facebook.com/docs/graph-api/reference/page/subscribed_apps/), [User accounts](https://developers.facebook.com/docs/graph-api/reference/user/accounts/), [debug_token](https://developers.facebook.com/docs/graph-api/reference/debug_token/), [Application subscriptions](https://developers.facebook.com/docs/graph-api/reference/application/subscriptions/), [Rate limiting](https://developers.facebook.com/docs/graph-api/overview/rate-limiting/), [Error handling](https://developers.facebook.com/docs/graph-api/guides/error-handling/), [Versioning](https://developers.facebook.com/docs/graph-api/guides/versioning/), [Webhooks, getting started](https://developers.facebook.com/docs/graph-api/webhooks/getting-started), [Webhooks for Pages](https://developers.facebook.com/docs/graph-api/webhooks/getting-started/webhooks-for-pages), [Graph v26.0 changelog](https://developers.facebook.com/docs/graph-api/changelog/version26.0), [Graph v26.0 announcement](https://developers.facebook.com/blog/post/2026/07/29/introducing-graph-api-v26-and-marketing-api-v26/), [Developer Policies](https://developers.facebook.com/devpolicy/), [Platform Terms](https://developers.facebook.com/terms/).

**WhatsApp Business Platform**

[Send messages](https://developers.facebook.com/documentation/business-messaging/whatsapp/messages/send-messages.md), [Image messages](https://developers.facebook.com/documentation/business-messaging/whatsapp/messages/image-messages.md), [Reaction messages](https://developers.facebook.com/documentation/business-messaging/whatsapp/messages/reaction-messages.md), [Location messages](https://developers.facebook.com/documentation/business-messaging/whatsapp/messages/location-messages.md), [Mark as read](https://developers.facebook.com/documentation/business-messaging/whatsapp/messages/mark-message-as-read.md), [Typing indicators](https://developers.facebook.com/documentation/business-messaging/whatsapp/typing-indicators.md), [Media](https://developers.facebook.com/documentation/business-messaging/whatsapp/business-phone-numbers/media.md), [Webhooks overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/overview.md), [Create a webhook endpoint](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/create-webhook-endpoint.md), [Override the callback URL](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/override.md), [Messages webhook reference](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/reference/messages.md), [Messages API reference](https://developers.facebook.com/documentation/business-messaging/whatsapp/reference/whatsapp-business-phone-number/message-api.md), [Pricing](https://developers.facebook.com/documentation/business-messaging/whatsapp/pricing.md), [AI providers pricing policy](https://developers.facebook.com/documentation/business-messaging/whatsapp/pricing/ai-providers.md), [Error codes](https://developers.facebook.com/documentation/business-messaging/whatsapp/support/error-codes.md), [Throughput](https://developers.facebook.com/documentation/business-messaging/whatsapp/throughput.md), [About the platform](https://developers.facebook.com/documentation/business-messaging/whatsapp/about-the-platform.md), [Get started](https://developers.facebook.com/documentation/business-messaging/whatsapp/get-started.md), [Access tokens](https://developers.facebook.com/documentation/business-messaging/whatsapp/access-tokens.md), [Permissions](https://developers.facebook.com/documentation/business-messaging/whatsapp/permissions.md), [Account model evolution](https://developers.facebook.com/documentation/business-messaging/whatsapp/account-model-evolution.md), [WhatsApp Business accounts](https://developers.facebook.com/documentation/business-messaging/whatsapp/whatsapp-business-accounts.md), [Phone numbers](https://developers.facebook.com/documentation/business-messaging/whatsapp/business-phone-numbers/phone-numbers.md), [Registration](https://developers.facebook.com/documentation/business-messaging/whatsapp/business-phone-numbers/registration.md), [Display names](https://developers.facebook.com/documentation/business-messaging/whatsapp/display-names.md), [Messaging limits](https://developers.facebook.com/documentation/business-messaging/whatsapp/messaging-limits.md), [Business-scoped user IDs](https://developers.facebook.com/documentation/business-messaging/whatsapp/business-scoped-user-ids.md), [Migrate a number to a business account](https://developers.facebook.com/documentation/business-messaging/whatsapp/solution-providers/migrate-existing-whatsapp-number-to-a-business-account.md), [Onboarding Business app users (coexistence)](https://developers.facebook.com/documentation/business-messaging/whatsapp/embedded-signup/onboarding-business-app-users.md), [Subscribed apps API](https://developers.facebook.com/documentation/business-messaging/whatsapp/reference/whatsapp-business-account/subscribed-apps-api.md), [Templates overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/overview.md), [Template components](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/components.md), [Data privacy and security](https://developers.facebook.com/documentation/business-messaging/whatsapp/data-privacy-and-security.md), [Getting opt-in](https://developers.facebook.com/documentation/business-messaging/whatsapp/getting-opt-in.md), [Groups](https://developers.facebook.com/documentation/business-messaging/whatsapp/groups.md), [Terms for the WhatsApp Business Platform](https://www.whatsapp.com/legal/business-solution-terms/), [WhatsApp Business Messaging Policy](https://business.whatsapp.com/policy).

Third-party tools named in Part 6 (check their own documentation): the Cloudflare quick tunnel page, the Tailscale Funnel page and the ngrok quickstart.

## Appendix D: Glossary

| Term | Meaning |
|---|---|
| **PSID** | Page-scoped ID: "An ID assigned to a person the first time the person sends a message to your Page." Unique to each user-and-Page pair. It is the recipient of a Messenger send and `counterpart.psid` in conversations |
| **ASID** | App-scoped ID, what Facebook Login gives you. It does not work with the Messenger Platform ("will not work"), so it produces "No matching user found" |
| **Page token** | A Page access token. It acts as the Page. Obtained from `GET /me/accounts` with a user token, or from the dashboard |
| **User token** | A person's access token. Short-lived about 1 to 2 hours, long-lived about 60 days (`fb_exchange_token`) |
| **App access token** | `APP_ID` and `APP_SECRET` joined with a pipe. Used for `debug_token` and app-level subscriptions |
| **System user** | A non-human user in Business settings whose tokens are meant for servers. Its token can be non-expiring or 60-day expiring |
| **WABA** | WhatsApp Business Account. Under the new account model it is the Messaging account (templates, billing, webhook subscriptions) and keeps its ID, used in `subscribed_apps`, `phone_numbers` and `message_templates` paths |
| **Phone number ID** | The ID of a business phone number (not the number). It is in the path of `/messages`, `/media` and `/register` |
| **BSUID** | Business-scoped user ID such as `US.13491208655302741918`, identifying a WhatsApp user within your portfolio. It can replace the phone number when a person adopts a username |
| **`wamid`, `mid`** | A WhatsApp message ID (`wamid...`) and a Messenger message ID (`m_...` or `mid...`) |
| **Standard / Advanced Access** | Whether a permission works only for people with an app role, or for everyone (App Review and Business Verification) |
| **Conversation Routing** | Meta's replacement for the Messenger Handover Protocol: which app owns a thread |
| **Tech Provider** | A business building apps used by other businesses. Out of scope here |
