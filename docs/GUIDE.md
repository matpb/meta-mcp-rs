# Implementation guide: from no Meta account to an assistant that reads and sends Messenger messages

This guide takes you from nothing (no Meta developer account) to an AI assistant that reads and
sends Facebook Messenger messages through meta-mcp. WhatsApp is the second track (Part 7). Parts 1
to 9 use Meta's official API. Your **own personal** Messenger and WhatsApp accounts are a separate,
unofficial route that needs none of that setup: Part 10, which you should read from its first section.

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
- [Part 10: Personal mode: your own Messenger and WhatsApp](#part-10-personal-mode-your-own-messenger-and-whatsapp)
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

**Personal accounts are a different route.** The "cannot" column is about the Graph API. meta-mcp also has a
**personal mode** that reaches your own Messenger and WhatsApp accounts through unofficial clients (a
re-implemented Messenger web client and a re-implemented WhatsApp linked device). It needs no Meta developer
account, app or token, it is outside Facebook's and WhatsApp's terms for personal accounts, and it can get the
account restricted or banned. It is Part 10, and 10.0 comes first. The Meta account, app, token and webhook steps of
Parts 1 to 9 do not apply to it.

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
that names the flag and exits with code 2. A login run that cannot ask must also name the Pages to
store (`--page-id`, repeatable, or `--all-pages`) and, for WhatsApp, the numbers (`--phone-number-id`);
Part 4.3 has the exact rules. To run the wizard again on tokens you already stored, without a new
sign-in, add `--stored`.

### 4.3 Every flag

Precedence for every value: **flag, then environment** (`META_APP_ID`, `META_APP_SECRET`,
`META_WEBHOOK_PUBLIC_URL`, `META_WEBHOOK_VERIFY_TOKEN`), **then the existing `credentials.json`,
then an interactive prompt.** For the App Secret that means `--app-secret`, `--app-secret-env` or
`--app-secret-file`, then `META_APP_SECRET`, then the secret already stored for the same App ID, then
a hidden prompt. `meta-mcp setup --help` prints the same flags; the table follows its order.

| Flag | Meaning |
|---|---|
| `--app-id ID` | The Meta App ID. Without it the wizard uses `META_APP_ID`, then the one in `credentials.json`, then asks |
| `--app-secret SECRET` | The App Secret. Accepted, but your shell history and the process list keep it, and the wizard warns |
| `--app-secret-env NAME` | Read the App Secret from the environment variable `NAME` (recommended). An unset variable is a usage error |
| `--app-secret-file PATH` | Read the App Secret from the first line of a file. The wizard warns when other users can read the file |
| `--login` | Sign in with Facebook Login in the browser. It is what a non-interactive run does when no token flag is given; an interactive run without a token flag first asks how to sign in (browser, pasted user token, pasted Page token, or the stored tokens when there are any) |
| `--user-token TOKEN` | Use a pasted user token (Graph API Explorer or System User) instead of signing in |
| `--page-token TOKEN` | Use a pasted Page token. This run stores no user token (one already stored stays) |
| `--stored` | Reuse the tokens already in `credentials.json` instead of signing in. The app, the Pages and the numbers are still checked, and the webhook step still runs. This is how you subscribe or re-subscribe the webhook later, for example after downtime (Parts 6.4 and 8.6). It stops when the file holds no token for this App ID |
| `--redirect-mode loopback\|paste` | `loopback` (default) or `paste` (Part 3.2) |
| `--redirect-uri URI` | Override the redirect URI in either mode. In loopback mode it must be a plain `http` address on `localhost` or `127.0.0.1` with an explicit port, and `--port` must agree with that port |
| `--port N` | Loopback or paste port, default 8766. A busy port is an error that names the fix |
| `--config-id ID` | Facebook Login for Business configuration ID (Part 3.3). It replaces the scope list |
| `--rerequest` | Re-ask permissions the person declined earlier (`auth_type=rerequest`) |
| `--no-browser` | Print the dialog URL and do not open a browser |
| `--page-id ID` | Store this Page; repeatable. With `--stored` it narrows the stored Pages, and an ID that is not stored is an error |
| `--all-pages` | Store every Page the login can manage |
| `--no-messenger` | Skip Messenger and Pages (a WhatsApp-only setup). Alone it is a usage error: it needs `--whatsapp` (or `--stored`) |
| `--replace` | Drop the stored Pages this run did not select, instead of keeping them. It also allows switching `credentials.json` to another app |
| `--whatsapp` | Also set up the WhatsApp Cloud API. `--whatsapp-token`, `--waba-id` and `--phone-number-id` imply it |
| `--whatsapp-token TOKEN` | A System User token for WhatsApp. Without it the wizard keeps the System User token already stored, else uses the user token |
| `--waba-id ID` | The WhatsApp Business Account ID |
| `--phone-number-id ID` | A business phone number ID to store; repeatable |
| `--instagram` | Also request the Instagram scopes (`instagram_basic`, `instagram_manage_messages`) |
| `--webhook-url URL` | The public webhook URL to subscribe (or `META_WEBHOOK_PUBLIC_URL`). Plain `http` is accepted for loopback hosts only, and the URL must be the bare callback, without `hub.*` parameters |
| `--webhook-override` | Point only the WhatsApp account at the webhook and leave the app-level callback alone (Part 7.5). The account gets a callback override, sent as a JSON body, and the app-level WhatsApp attempt is skipped |
| `--verify-token TOKEN` | The webhook verify token (or `META_WEBHOOK_VERIFY_TOKEN`). Generated (32 random bytes in hex) when none is given or stored |
| `--non-interactive` | Never prompt; a missing value is a usage error (exit code 2) |
| `--timeout-seconds N` | How long to wait for the sign-in callback or the pasted URL, default 300 |
| `-y`, `--yes` | Answer yes to the confirmations: replacing a different webhook callback, switching apps, and continuing when the current callback cannot be read |
| `-h`, `--help` | Print the help (`-h` is the short summary) |

Some flags exclude each other, and the command line is then refused with exit code 2: `--app-secret`,
`--app-secret-env` and `--app-secret-file` (at most one); `--login`, `--user-token`, `--page-token` and
`--stored` (at most one); `--page-token` with `--no-messenger`; and `--page-id`, `--all-pages` and
`--no-messenger` (at most one of the three).

**Runs that cannot ask.** With `--non-interactive`, or whenever stdin is not a terminal, nothing is
prompted and a missing value is a usage error (exit code 2) that names the flag. Two selections are
checked before the sign-in, so that it is not wasted:

- A run that signs in (the default, `--login` or `--user-token`) and keeps Messenger needs `--page-id`
  (repeatable) or `--all-pages`, because the sign-in lists the Pages and nobody can pick. A run with
  `--page-token`, `--stored` or `--no-messenger` needs no Page selection.
- A run that sets up WhatsApp needs `--phone-number-id` (repeatable), unless the numbers already stored
  for this App ID can be reused: that is the case when `credentials.json` holds numbers and you did not
  pass `--waba-id`.

**Confirmations.** The wizard asks before it does something that is hard to take back: replacing a
webhook callback the app already uses for another URL, replacing everything stored when
`credentials.json` belongs to another app, and continuing when it cannot read which callback the app
has today. `--yes` answers yes to those three (`--replace` also covers the app switch). Without it a
non-interactive run answers no and stops with a message that names the flag. Optional extras, such as
"Also set up WhatsApp?" or "Subscribe to webhooks now?", are offered only to a person at the terminal:
`--yes` does not accept them.

**Logs.** `setup`, `doctor` and `token` log to stderr at `meta_mcp=warn` unless `META_MCP_LOG` says
otherwise, and they always switch off the logs of the HTTP and TLS libraries, because the OAuth requests
carry the App Secret in their URLs. The login URL is printed in full; secrets never are.

### 4.4 What each step does and verifies

The wizard prints a `Step N: <title>` header, then `✅` (done), `⚠️` (warning), `❌` (failed) and `ℹ️`
lines, with no colour codes, and every `❌` is followed by a `Fix:` line. Steps are numbered as they
run, so a step the run does not need is not printed.

| Step | What it does | What it verifies |
|---|---|---|
| App | Resolves the App ID and App Secret | `GET /{app-id}?fields=id,name,link` with the app token proves the pair, and prints `✅ App <name> (<id>)`. On error 190 or 101: `❌ App ID or App Secret rejected: Settings > Basic in the App Dashboard`. If the rejected secret came from `credentials.json` (was it reset?), an interactive run asks for the current one and a non-interactive run stops and names `--app-secret-env`. If `credentials.json` belongs to another App ID, the run stops unless you pass `--replace` or `--yes` (or answer yes), because switching apps drops the stored Pages, tokens, WhatsApp account and webhook settings |
| Sign in with Facebook, User token, Page token or Stored tokens | Obtains the token by the route you chose (`--login`, `--user-token`, `--page-token` or `--stored`) | Login: `state` is 32 random bytes in hex and is compared in constant time; the code is exchanged for a short-lived token and then a long-lived one; `debug_token` records scopes and expiry. Missing scopes print `⚠️ missing scope X: rerun with --rerequest`, which is fatal only when `pages_messaging` is missing in a Messenger setup. Pasted user token: `debug_token` refuses a Page token, and a token that expires within two hours is exchanged for a long-lived one. Pasted Page token: `debug_token` refuses any other token type, warns when `pages_messaging`, `pages_manage_metadata` or `pages_read_engagement` is missing or when the token expires, and `GET /me` names the Page. Stored tokens: nothing is requested, and the step stops when `credentials.json` holds no token for this App ID. When a login would ask for no permission at all (Messenger off, and a stored System User token serves WhatsApp), the sign-in is skipped with `ℹ️ no sign-in is needed` |
| Pages | Lists the Pages (`GET /me/accounts`, paginated), selects (flags or a numbered prompt) and stores each Page token | Each Page: `GET /{page-id}?fields=id,name` and `GET /{page-id}/conversations?limit=1&platform=messenger`. A result of code 10, 200 or 230 prints `⚠️` with the permission and task names. With `--stored` the stored Pages are re-checked and no list is fetched; with `--page-token` the one Page is checked. A login that manages no Page continues with WhatsApp when WhatsApp is on |
| WhatsApp | Uses `--whatsapp-token`, else the System User token already stored, else the user token. Discovers businesses, WhatsApp Business Accounts and phone numbers, or skips discovery when a System User token comes with `--waba-id` and `--phone-number-id`, or reuses the stored numbers when you pass neither | Each number: `GET /{phone-number-id}?fields=display_phone_number,verified_name,quality_rating,code_verification_status,status`. Warns that a user token expires in about 60 days, and when a System User token expires within 7 days. A stored System User token that no longer works is replaced by the user token if that has the WhatsApp permissions; otherwise the step stops and names `--whatsapp-token` |
| Webhook | Skipped, with the command to run later, unless a URL comes from `--webhook-url`, `META_WEBHOOK_PUBLIC_URL` or the prompt. Then it settles the verify token, checks that the URL is reachable and subscribes at app, Page and WhatsApp level | The reachability self-check, then read-backs of the subscriptions. A callback the app already uses for another URL is replaced only after a yes or `--yes`. Detail in Part 6.4 |
| Credentials | Writes `credentials.json` by merging into the existing file; Pages the run did not touch are kept unless you pass `--replace` | Prints the path and its permissions |
| Connect your client and check everything | Prints client snippets and runs the doctor | Exits with the doctor's code |

**Progress is saved as it goes.** The token, Pages and WhatsApp steps each write `credentials.json` as
soon as they have something to keep, the webhook step stores the verify token before the reachability
check and the URL after a subscription took, and the Credentials step writes the final merge. When a
step fails after something was saved, the wizard prints `ℹ️ progress so far is saved in <path>`, so
the rerun (or `--stored`) does not start from nothing.

**The verify token.** A run that finds no verify token anywhere (not in `--verify-token`, not in
`META_WEBHOOK_VERIFY_TOKEN`, not in `credentials.json`) creates one, 32 random bytes as 64 hex
characters, even when no webhook URL was given, so that `meta-mcp webhook` can start. It is created
once, kept on later runs and never printed.

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
| `user_token` | `null` when none was ever stored (for example after a first run with `--page-token`, which stores no user token). `expires_at` is Unix seconds or `null`; `scopes` are what `debug_token` reported |
| `pages` | One entry per stored Page, each with its own non-expiring Page token (when derived from a long-lived user token) |
| `whatsapp` | `null` without WhatsApp. `token_kind` is `user` or `system_user` |
| `webhook` | The verify token and the public URL |

An empty `{}` file loads as an empty configuration.

### 4.6 `meta-mcp doctor`

```bash
meta-mcp doctor            # exit 0 when no check failed, 1 otherwise
meta-mcp doctor --json     # {"checks": [...], "ok": true|false, "summary": {...}}
meta-mcp doctor --webhook  # also test that the public callback answers the verification handshake
```

The summary line reads `N ok, N warnings, N failures`. Each check is a status (`✅`, `⚠️`, `❌`, or `ℹ️`
for information, which the counts leave out), a name, a detail, and for anything not green a `Fix:` line.
Every network check has a 15-second timeout and a failure never crashes the command. Tokens and the app
secret are never printed. A configuration error, for example a bad `META_MCP_FILES_DIR`, shows up as one
failing `config` check. The `--json` document has three keys: `checks` (each with `name`, `status` as
`ok`, `warn`, `fail` or `info`, `detail`, and `fix`, which is `null` when there is none), `ok` (true when
nothing failed) and `summary` (the `ok`, `warnings` and `failures` counts). Line by line:

| Check | Calls and looks at | Warning when | Failure when |
|---|---|---|---|
| `setup` | Whether any Page or WhatsApp number is configured | | None is (fix: `meta-mcp setup`) |
| `app` | `GET /{app-id}?fields=id,name` with the app token | The App ID or App Secret is unknown | The app token is rejected, or the call fails |
| `credentials` | The mode of `credentials.json` | The file is missing, or its mode is not 0600 (the fix offers `chmod 600`) | |
| `user token`, `user scopes` | `debug_token`: validity, days left, scopes | Under 10 days left (fix: `meta-mcp token refresh`); a scope is missing | The token is invalid or expired (fix: `meta-mcp setup`) |
| Each Page | `debug_token` (type, expiry, scopes), `GET /{page-id}?fields=id,name`, the conversations probe, `subscribed_apps` fields | The conversations probe is refused or fails; a webhook is configured but `messages` is missing from the subscribed fields (fix: `meta-mcp setup --stored --webhook-url ...`) | The token is invalid or a call fails; the line names `meta-mcp setup` |
| `app subscription` | `GET /{app-id}/subscriptions`, when a public URL is stored and a Page is configured | There is no `object=page` subscription, it calls another URL, lacks `messages` or is inactive | |
| WhatsApp | The token with `debug_token`, each phone number, and the WABA's `subscribed_apps` (does it list this app, and does a callback override send events elsewhere) | The token is close to expiry, the app is not subscribed while a webhook is configured, or an override points elsewhere | The token is invalid or a number cannot be read |
| `webhook`, `webhook reachability` | The stored public URL and verify token; with `--webhook`, our own verification GET against the callback | A public URL is stored without a verify token | With `--webhook`, the callback does not answer with the challenge |
| `inbox` | Opens the database; counts and the age of the last event | A webhook is configured and no event arrived in 7 days ("send a test from the dashboard"), or there is no database yet | The database cannot be opened |
| `files`, `mode`, `access` | The files directory is writable, the read-only flag, and (when a Page is configured) the gate test of Part 2.5 as a reminder | The files directory is not writable | |

### 4.7 The token commands

```bash
meta-mcp token refresh              # re-exchange the stored user token and re-derive Page tokens
meta-mcp token refresh --whatsapp   # also refresh the stored WhatsApp token
meta-mcp token import --page-token TOKEN
meta-mcp token import --user-token TOKEN
meta-mcp token import --whatsapp-token TOKEN --phone-number-id ID --waba-id ID
meta-mcp token show                 # kinds, expiry, scopes, names; never a token value
meta-mcp token show --json
```

`token refresh` needs the App ID and App Secret (from `credentials.json` or the environment). It trades
the stored user token for a fresh long-lived one (`fb_exchange_token`), stores the new token and expiry,
and replaces the stored Page tokens of the same Pages (a Page that `me/accounts` no longer returns keeps
its old token and gets a warning). A WhatsApp token that is the user token follows it. `--whatsapp` also
handles a separate stored WhatsApp token: it asks `debug_token` first, leaves a token that never expires
alone ("nothing to refresh"), stops on one that is no longer valid, and otherwise exchanges it, with
`set_token_expires_in_60_days=true` for a System User token. It still runs when the user token cannot be
refreshed, and what was refreshed and saved before a failure stays saved. With no stored user token,
`token refresh` stops and points at `meta-mcp setup --login` and `token import --user-token`;
`--whatsapp` alone can still refresh a WhatsApp token.

`token import` stores exactly one kind per run: `--page-token`, `--user-token` or `--whatsapp-token`
(none, or more than one, is a usage error). `--phone-number-id` and `--waba-id` go with `--whatsapp-token`
only. It checks the value with `debug_token` when the app secret is known (an invalid token is refused
and nothing is stored, and so is a `--page-token` that is not a Page token), asks `GET /me` who owns
it, checks a phone number with the token, and then stores it in `credentials.json`.

`token show` prints the stored kinds, issue and expiry dates, scopes, Page and number IDs and names,
and whether the app secret and the verify token are set. It never prints a token value, the app
secret or the verify token; `--json` gives the same facts as one document.

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
   file in `META_MCP_FILES_DIR`. `messenger_download_attachment` saves one the customer sent, into
   `inbound/messenger/` in that directory. Files under `inbound/` came from other people, so
   sending one on needs `allow_inbound=true` on the send call; pass it only when the user asked to
   forward that exact file. Meta documents no supported-format list for Messenger attachments
   (UNVERIFIED), and the docs do not say whether text and an attachment can share one message
   (UNVERIFIED), so send them as two calls.
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
meta-mcp tries the URL without a token and retries once with the Page token after a 401 or 403, only for
`*.fbcdn.net` and `*.fbsbx.com` hosts (or hosts you listed in `META_MCP_DOWNLOAD_HOSTS`) and only for
the exact host and port of that URL. It refuses to fetch the Graph API host at all, and it shows a
saved picture to the assistant only when the bytes themselves parse as a PNG, JPEG, GIF or WEBP
picture of at most 2 MiB. The saved file's name comes from your `dest_name`, the attachment's own name
or the URL, never from a `Content-Disposition` header, and a name that clashes with different content
gets a short URL hash.

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

**Do not use `HUMAN_AGENT` for AI-written replies.** It is for a person replying by hand in a custom
inbox, and Meta lists automated messages as a disallowed use of it. meta-mcp says so in the
descriptions of the Messenger send tools, in the server instructions and in the hint it attaches to
an outside-window error, so the assistant is told in three places. Inside the window, AI replies are
normal. Outside it, Meta's documented routes are utility messages (a pre-approved template sent with
`pages_utility_messaging`, which "must not contain marketing materials"), Marketing Messages,
One-Time Notification and Sponsored Messages. None of them has a meta-mcp tool.

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
| Conversations API | 2 calls per second per Page (meta-mcp spaces conversation reads at least 500 ms apart per Page, and repeats a read that failed with a transient error once, after the same spacing) |
| Send API: text, links, reactions, stickers | 300 calls per second per Page |
| Send API: audio or video | 10 calls per second per Page |
| 24-hour cap | `200 * Number of Engaged Users` calls |
| One thread | "may be rate limited if too many messages are sent to a single thread" |
| High volume | Above 40 messages per second, calls that list all conversations fail; single-conversation reads still work |

Error codes: 4 (app limit), 17 (user token limit), 32, 613 (custom limit; 613/2534040 "Calls to this
api have exceeded the rate limit"), 80001 (Pages calls with a Page token), 80006 (Messenger: "too
many messenger api calls to this Page account"), 341 (application limit), 368 (temporarily blocked
for policy), 1 and 2 (temporary). Meta: "When the limit has been reached, stop making API calls.
Continuing to make calls will continue to increase your call count." meta-mcp's Graph client retries
reads twice (400 ms, then 1200 ms, plus jitter) on 1, 2, 4, 17, 32, 613, 80006 and 130429, and never
retries a send. Messenger conversation reads (`messenger_list_conversations`,
`messenger_read_conversation`, `messenger_get_message` and the lookups behind a `conversation_id` or
`message_id`) are paced instead: the client's retries are off for them, calls on one Page are spaced
500 ms apart, and a transient failure (Graph code 1, 2, 4, 17, 32, 613 or 80006, an HTTP 5xx, a
timeout or a connection that did not complete) is repeated once through the same spacing, so a repeat
never lands inside the 500 ms window.
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

The receiver must already answer on the public URL (6.2 and 6.3), because Meta calls it while you
subscribe. On a first setup, add the URL to the wizard run:

```bash
meta-mcp setup --webhook-url https://hooks.example.org/webhook
```

Once `credentials.json` holds your tokens, subscribe or re-subscribe **without a new sign-in** by adding
`--stored`:

```bash
meta-mcp setup --stored --webhook-url https://hooks.example.org/webhook
```

`--stored` reuses the stored App ID, App Secret, user token, Page tokens and WhatsApp account, checks
them again, and repeats the subscription steps below, which are idempotent: run it as often as you need,
for example after a long outage (6.5). Without `--stored` the wizard signs you in again unless you pass
`--user-token` or `--page-token`. With `META_WEBHOOK_VERIFY_TOKEN` exported it uses the token your
receiver already has. If the reachability check fails, the wizard prints the exact `--stored` command
to rerun once the receiver answers, with the flags that shaped the run. Steps:

1. **Verify token.** `--verify-token`, else `META_WEBHOOK_VERIFY_TOKEN`, else the stored one, else a
   new 32-byte hex value. It is saved to `credentials.json` before anything is sent to Meta.
2. **Reachability self-check.** It sends our own handshake to your URL (`GET` with `hub.mode`,
   `hub.verify_token` and a random `hub.challenge`, 10-second timeout) and needs 200 with the
   challenge as the body. If it fails, setup stops, says why (403: the verify token differs; 404:
   check the path; a redirect: use the final URL; 502 to 504: the tunnel is up but nothing answers
   behind it) and tells you to start `meta-mcp webhook` and the tunnel, then rerun. **Why:** Meta
   sends the same `GET` while you subscribe, so the receiver must already be reachable. Plain `http`
   URLs are accepted for loopback hosts only.
3. **App level.** The wizard first reads what the app sends for `object=page` today. If that is a
   callback for a different URL it asks "Replace it?" (default no; `--yes` answers yes), and a
   non-interactive run without `--yes` stops and leaves the old callback in place. Then it calls
   `POST /{app-id}/subscriptions` with the app token: `object=page`, `callback_url`, the field list,
   `verify_token`, `include_values=true`.
4. **Page level.** `POST /{page-id}/subscribed_apps` with the Page token and `subscribed_fields` set
   to the same list. Meta: "only fields with subscriptions at both the page and app levels will get
   Webhooks."
5. **WhatsApp**, when configured. Without `--webhook-override` it tries `POST /{app-id}/subscriptions`
   with `object=whatsapp_business_account`, `fields=messages` (after the same "Replace it?" question
   for that object). Meta's docs describe that call, but its Graph reference says WhatsApp objects must
   be configured in the dashboard, so on an error the wizard prints the dashboard steps (Part 7.5) and
   carries on; if you decline to replace a different app-level callback, it leaves the account alone
   and suggests `--webhook-override`. Then it subscribes the WABA: `POST /{waba-id}/subscribed_apps`.
   With `--webhook-override` the app-level attempt is skipped and the app-level callback stays
   untouched; the WABA call then carries a JSON body with `override_callback_uri` and `verify_token`,
   so only that account's events go to your URL.
6. **Read-back.** It reads `GET /{app-id}/subscriptions`, `GET /{page-id}/subscribed_apps` and the
   WABA's `subscribed_apps`, and prints the fields that are active, with a warning for an inactive
   subscription, a missing field or an override that points elsewhere.

The URL is recorded in `credentials.json` only once at least one subscription of the run went through.
If none did, setup stops with `no subscription of this run sends events to <url>, so it was not
recorded` and suggests `--webhook-override` (only the WhatsApp account moves) or `--yes` (replace the
existing callback).

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
  **Re-run `meta-mcp setup --stored --webhook-url ...`** (no new sign-in) after any outage longer than
  an hour.
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
`object=whatsapp_business_account` (unless you pass `--webhook-override`), which Meta's WhatsApp docs
describe but whose Graph reference says is dashboard-only. When it fails the wizard prints these steps.

**Overrides.** Precedence is: phone number override, then Messaging account override, then the app's
callback URL. `meta-mcp setup --webhook-url URL --webhook-override` registers URL as a Messaging
account override (`override_callback_uri` and `verify_token` as the JSON body of `POST
/{waba-id}/subscribed_apps`) and skips the app-level attempt, so the app's own callback stays as it is. A
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
ways. Free-form sends (text, media, location) look in the local inbox, after the send, for the newest
message from the person you wrote to, under every ID the person is known by (the number or user ID
you used, the `wa_id` and `user_id` Meta answered with, and the user and parent user IDs kept in the
inbound event), and add a `window_warning` when the window looks closed; it never blocks the send.
And the error hint says to send a template. When a send ends in a timeout, a dropped connection, an
unreadable answer or an HTTP 5xx, its outcome is open: the error says the message may still have been
delivered, so check the inbox status events before you send again.

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
explicitly, because a mismatch (error 131053) is common. The file extension decides the type:
`mime_type` may only restate it (with parameters such as `codecs=opus`) and cannot stand in for a
missing or unknown extension. A file under `inbound/`, where downloads from other people are kept,
is refused unless the call says `allow_inbound=true`. If a send fails after an upload, the error
names the media ID: after a definite rejection you can resend with `media_id` (valid for 30 days),
and after an open outcome (a timeout or an HTTP 5xx) check the inbox status events first. The limits
are in [TOOLS.md](TOOLS.md#whatsapp_send_media). Documents outside Meta's list "may be sent via the
API, but they are not supported and may not be handled as expected".

### 7.10 Receiving through the inbox

Inbound messages arrive as `message` events: text, image, audio (with a voice flag), video, document
(with `filename`), sticker, location, contacts, interactive replies (`button_reply`, `list_reply`),
template quick-reply `button` taps, reactions, orders, `unsupported` types (polls, for instance, arrive
as `unsupported` with error 131051 or 131060), edits (within 15 minutes) and revokes (within two days).
Media events carry the media `id`, MIME type and a base64 `sha256`. Each message you send produces up to
three `status` events (`sent`, `delivered`, `read`), or `failed` with error details.

To fetch media use `whatsapp_download_media` with the event attachment's `media_id`: it asks Graph for
a fresh URL, which expires after 5 minutes, and downloads it with your token, which Meta requires. A
404 means the URL expired. The file is saved in `inbound/whatsapp/` of `META_MCP_FILES_DIR`, named
`whatsapp_<media id>.<extension>` unless you give `dest_name`, and the result carries a picture inline
only when its bytes parse as a PNG, JPEG, GIF or WEBP of at most 2 MiB (`inline_skipped` says why not).
Forwarding the file with a send tool needs `allow_inbound=true`. `whatsapp_mark_read` sets the blue
ticks, and with `typing` shows a typing indicator for up to 25 seconds, so use it only when a reply
follows.

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

- **Read-only.** `META_MCP_READ_ONLY=true` removes the 21 tools that send or change state: 11 business tools and
  the 10 personal write tools (list in [TOOLS.md](TOOLS.md#read-only-mode)). Start here and open up
  deliberately. The download tools stay; the two business ones refuse `overwrite=true` there
  ([SECURITY.md](../SECURITY.md#read-only-mode)). The personal daemon is a separate process and needs the
  variable too (Part 10.4).
- **File jail.** `META_MCP_FILES_DIR` is the only place file tools read and write. Use an empty,
  dedicated directory: meta-mcp refuses to start if it contains `META_MCP_HOME`, `credentials.json` or
  the inbox database, and the error says which. Downloads are saved under `inbound/messenger/` and
  `inbound/whatsapp/` in it (and `inbound/messenger_personal/` and `inbound/whatsapp_personal/` for personal
  mode), and the upload tools refuse files there unless `allow_inbound` is true.
  Read the limits in [SECURITY.md](../SECURITY.md#file-jail). `META_MCP_MAX_FILE_BYTES` caps file size
  (default 100 MiB).
- **Logs.** They go to stderr (journald under systemd). `META_MCP_LOG` takes tracing filter syntax;
  the default is `meta_mcp=info` (`meta_mcp=warn` for `setup`, `doctor` and `token`). Use
  `meta_mcp=debug` to see retries and `meta_mcp=trace` for the most detail. Tokens and the app secret
  are never logged, webhook bodies are not logged (batch counts are), and error text is scrubbed of
  anything shaped like a Graph token.

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
longer than that, start the receiver again and run
`meta-mcp setup --stored --webhook-url https://meta.example.org/webhook`. `--stored` reuses the tokens
in `credentials.json`, so there is no new sign-in, and the subscription steps are idempotent. Add
`--webhook-override` if you use a WhatsApp callback override, and `--yes` to replace a callback the app
already has for another URL. Then run `meta-mcp doctor --webhook` to confirm. Meta cannot replay what
you missed, and a Messenger subscription that was dropped delivers nothing until you subscribe again.
For WhatsApp Meta keeps retrying for up to 7 days (36 hours on the generic Graph page), so a receiver
that comes back catches up inside that window.

### 8.7 Rotating the app secret

1. In the App Dashboard open **Settings > Basic** and reset the App Secret. Only an Administrator can.
   Meta: "It is not possible to programmatically rotate the app secret."
2. Update every place that holds it: `credentials.json` (rerun `meta-mcp setup` with the new secret in
   the environment, or edit `app_secret`) and `META_APP_SECRET` in your environment files, for the
   webhook process and the MCP server alike.
3. Restart. Until you do, webhook signatures fail with 401 and, with **Require App Secret** on, calls
   fail for lack of a valid `appsecret_proof`.
4. Run `meta-mcp doctor --webhook`, then `meta-mcp setup --stored --webhook-url ...` if deliveries
   stopped.

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
Meta support asks for) and, for known codes, a hint. Troubleshooting for personal mode is in
[Part 10.6](#106-troubleshooting-personal-mode).

### 9.1 Graph errors by code and subcode

Messenger and general Graph codes (Meta's error tables as documented on 2026-10-24):

| Code / subcode | Cause | Fix |
|---|---|---|
| 190 (subcodes 458 app not installed, 459 user checkpointed, 460 password changed or logged out, 463 expired, 464 unconfirmed user, 467 invalid) | The token is dead | `meta-mcp token refresh` while the user token is valid, else `meta-mcp setup`. A Page token from a short-lived user token lasts only 1 hour |
| 10, 200 to 299, 230 | Permission or Page task missing, or app role missing (200 / 2018028 and the window subcodes below have their own hints) | Check scopes with `meta-mcp token show` or `debug_token`; the person needs the `MESSAGING` and `MODERATE` tasks (Part 2.1); in Development mode only role users can be reached |
| 200 / 2018028 | "Cannot message users who are not admins, developers or testers of the app until pages_messaging permission is reviewed and the app is live." | Give the person a Tester role, or run the gate test (Part 2.5). meta-mcp has a hint of its own for this subcode: the app is not approved yet, so the Page can only message people with a role on the app |
| 10 / 2018278, 10 / 2534022 | "This message is sent outside of allowed window." | The person must message the Page again. There is no tag for AI replies (Part 5.2): `HUMAN_AGENT` is only for a person replying by hand. meta-mcp keys its window hint on these two subcodes whatever the code |
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
| 131030 | Recipient not in the allowed list of a test number (community reports; **UNVERIFIED**). meta-mcp's hint names no limit on the number of recipients | Add the number to the recipient list (Part 7.2) |
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
| `App ID or App Secret rejected: Settings > Basic in the App Dashboard` | `GET /{app-id}` failed with 190 or 101 | Copy both again from **Settings > Basic**; check **Settings > Advanced > App Type** is not Native/Desktop (Part 1.7). If the secret came from `credentials.json` and was reset, pass the current one with `--app-secret-env NAME` (an interactive run asks for it) |
| `credentials.json belongs to another app: pass --replace (or --yes) to switch apps, or check --app-id and META_APP_ID` (exit code 2) | The stored file is for a different App ID, and switching drops its Pages, tokens, WhatsApp account and webhook settings | Check the App ID; pass `--replace` or `--yes` only if you mean to start over for the new app |
| `no callback received within N seconds` | The sign-in did not reach the listener within `--timeout-seconds`. The `Fix:` line asks whether the redirect URI is listed, exactly, under **Valid OAuth Redirect URIs** | Register the exact URI (Part 3.2); or use `--redirect-mode paste` |
| `port N is already in use` | Something else uses the loopback port (8766 by default) | Free it, or pass `--port N` and register that URI, or use `--redirect-mode paste` |
| A state mismatch: HTTP 400 on the callback, or `the state in the pasted URL does not match this sign-in attempt` | The callback did not carry the `state` this run created | Rerun the wizard and use the new URL it prints. In loopback mode a wrong-state request is refused and counted, and the timeout message says how many |
| `the sign-in did not finish: Facebook reported user_denied` | The sign-in was cancelled | Rerun and approve |
| `missing scope X: rerun with --rerequest` | The person declined a permission | Rerun with `--rerequest`. Fatal only when `pages_messaging` is missing in a Messenger setup (`pages_messaging was not granted`) |
| `exchanging the login code failed` | Meta refused the code exchange | The redirect URI must be byte-identical to the one the wizard printed and be listed under **Valid OAuth Redirect URIs**; codes work once, so rerun the wizard |
| `missing value: pass --page-id ID (repeatable) or --all-pages ...` (exit code 2) | A non-interactive login run with no Page selection | Pass one of them (Part 4.3) |
| `missing value: pass --phone-number-id ID (repeatable) ...` (exit code 2) | A non-interactive WhatsApp run that cannot reuse stored numbers | Pass the numbers |
| `there are no stored tokens for this app yet` | `--stored` on a file without a token for this App ID | Run `meta-mcp setup --login` first (`--login --replace` when the file belongs to another app) |
| `Page X is not stored (stored: ...)` | `--stored --page-id X` names a Page that is not in `credentials.json` | Pick a stored ID, or add the Page with `meta-mcp setup --login --page-id X` |
| A per-Page warning with a Graph hint | The conversations probe failed, usually code 10, 200 or 230 | Page tasks and permissions (Parts 2.1, 1.3) |
| `the callback URL is not reachable: ...` | The receiver or the tunnel is down, or the URL is wrong; the message says which | Start `meta-mcp webhook` and the tunnel, then run the `meta-mcp setup --stored --webhook-url ...` command the message prints (Part 6.4) |
| `left the existing page webhook callback in place` | The app already sends events to another URL, and the "Replace it?" question was answered no or could not be asked | Rerun with `--yes` to replace it, or look at **App Dashboard > Webhooks** first |
| `no subscription of this run sends events to <url>, so it was not recorded` | Every subscription failed or was left alone | Rerun with `--webhook-override` (only the WhatsApp account moves), or with `--yes`, or fix the warning above it |
| The WhatsApp app-level subscription errors | Meta's Graph reference says WhatsApp objects are dashboard-only | Follow the dashboard steps the wizard prints (Part 7.5) |
| A usage error naming a flag, exit code 2 | `--non-interactive` and a missing value, flags that exclude each other, or an invalid ID | Pass the flag or its environment variable, or drop the conflicting flag |
| `--webhook-url` refused | Plain `http` to a non-loopback host, credentials or a fragment in the URL, or `hub.*` parameters | Use the bare `https` callback URL |
| `configuration problem: ...` | A startup setting is invalid, for example `META_MCP_FILES_DIR (...) contains META_MCP_HOME (...)` | Fix the setting the message names. For a corrupt `credentials.json` the `Fix:` line prints the exact `mv` command |

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
8. The app has not been unsubscribed after an hour of failures: re-run
   `meta-mcp setup --stored --webhook-url ...` (Part 6.5). The dashboard's **Messenger > Settings >
   Webhooks card > Show recent errors** lists them.
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


## Part 10: Personal mode: your own Messenger and WhatsApp

Parts 1 to 9 put an assistant on a Facebook Page and a WhatsApp business number through Meta's official
API. This part is a different route: the assistant reads and sends on **your own personal accounts**, the
Messenger and the WhatsApp you use on your phone, the way you do. Meta offers no API for that (Part 0.1), so
personal mode uses **unofficial clients**. Read 10.0 before anything else: it says what that can cost.

You do not need a Meta developer account, an app, a token, `meta-mcp setup`, a Page, a public URL or a
webhook. Personal mode runs on one machine that you control. What it shares with the earlier parts is the
binary, the MCP server, the inbox and the files directory (`META_MCP_FILES_DIR`).

**How to read it.** Statements about Facebook, Messenger and WhatsApp come from the sidecar documentation in the
repository (`sidecars/messenger/docs/SIDECAR-MESSENGER.md` and `sidecars/whatsapp/docs/SIDECAR-WHATSAPP.md`) and
the research behind it; page text is quoted as rendered on 2026-10-24. **UNVERIFIED** keeps its meaning from the
rest of the guide: no first-party text settles the point. Commands assume `meta-mcp` is on your `PATH`.

- [10.0 What this is and is not](#100-what-this-is-and-is-not)
- [10.1 Install](#101-install)
- [10.2 Messenger login](#102-messenger-login)
- [10.3 WhatsApp login](#103-whatsapp-login)
- [10.4 Using it](#104-using-it)
- [10.5 Operations](#105-operations)
- [10.6 Troubleshooting](#106-troubleshooting-personal-mode)
- [10.7 Command reference and sources](#107-command-reference-and-sources)

### 10.0 What this is and is not

**What it is.** Two unofficial clients. Each is a Go program, called a *sidecar*, that a long-lived Rust process,
the *personal daemon*, starts and talks to over pipes (Part 10.1 installs it).

| | Messenger | WhatsApp |
|---|---|---|
| Program | `meta-messenger-sidecar` | `meta-whatsapp-sidecar` |
| How it logs in | With the cookies of a browser session on facebook.com | It registers itself as a linked device on your own account, by QR code or a code typed on the phone |
| Library | `messagix` from the open-source mautrix-meta bridge, a re-implementation of Messenger's web protocol, not an SDK. End-to-end encrypted chats go through a second client inside it, `whatsmeow` | `whatsmeow`, a re-implementation of the WhatsApp Web protocol |
| What it keeps on disk | The cookies, an encrypted-chat device with a cache of decrypted messages, a thread map, an attachment index | The device keys, and its own copy of your messages, because the library keeps none |
| Your phone | Not part of the login. The code has no phone approval step; whether Meta notifies the phone about the encrypted-chat device is **UNVERIFIED** | Stays the primary device. Linked devices log out if the phone is unused for over 14 days |

Meta says it "is in the process of securing personal messages with end-to-end encryption by default" on
Messenger, so both encrypted and unencrypted chats exist. Whether the rollout is complete is **UNVERIFIED**,
and the Messenger sidecar handles both.

**What it is not.** It is not the Graph API, not a bot account and not business mode. It acts as you, from your
Facebook identity and your phone number, and what it sends goes out in your name.

**It is outside the platforms' terms.** For personal accounts, Facebook and WhatsApp do not allow unofficial
clients, and the sidecars are exactly that. Quotes below are as the research read them on 2026-10-24; none of it
is a prediction about your account, and none of it is legal advice.

| Platform | What is documented | Where it was stated |
|---|---|---|
| Facebook | "You may not access or collect data from our Products using automated means (without our prior permission) ... regardless of whether such automated access or collection is undertaken while logged-in to a Facebook account." (Terms, section 3.2). Section 4.2 allows Meta to "suspend or permanently disable your access". The Automated Data Collection Terms (effective 2024-10-07) say "You will not engage in Automated Data Collection without first obtaining Meta's express written permission" | Facebook Terms; Automated Data Collection Terms |
| Facebook | "In some cases, Meta may decide your account has suspicious activity and block you until you do some tasks like completing a captcha, adding a phone number or resetting your password. It is recommended to have two-factor authentication enabled to reduce the risk of such blocks." | mautrix-meta authentication documentation |
| Facebook | Users of the library report "We suspect automated behaviour on your account", soft locks, a bridge token blocked within 24 hours of re-adding it, and `meta-lserror-1357053: Your account is disabled` (an issue open since 2024-03-22). Results are mixed: one user ran 7 months before a flag, another is flagged every one to two months. User reports, not statements from Meta | mautrix-meta issue tracker |
| Facebook | A contributor reports that about half of new logins were revoked within minutes after the library added a secure-history PIN step (a 418 on the encrypted socket, then the `xs` cookie deleted, sometimes a "hacked-protection" checkpoint), also for people who never entered the PIN. The cause is **UNVERIFIED**. The sidecar never calls that step (10.2) | mautrix-meta pull request 425 |
| WhatsApp | "Using these apps or linking your WhatsApp account to unofficial versions of WhatsApp violates our Terms of Service." "Your WhatsApp account might also be temporarily or permanently banned, or it could lead to restrictions on your account, including the ability to link devices." | WhatsApp Help Center, unofficial apps |
| WhatsApp | "Linking your account to an unofficial app or website, now or in the past, may result in a temporary or permanent account ban." The words "now or in the past" make the exposure retroactive | WhatsApp Help Center, linked devices |
| WhatsApp | A banned number shows "This phone number can't use WhatsApp", and "You can't access your chat history or backups while your account is banned." A review can be requested in the app ("Most reviews are completed within 24 hours"); "If you don't see an option to request a review, it means the ban is final". The ban lands on the account on your phone, not only on the linked device | WhatsApp Help Center, account bans |
| WhatsApp | A restricted account "can't start chats or calls with new contacts, create new groups, add new members to groups, or use linked devices. You may also be logged out of some companion devices." | WhatsApp Help Center, restricted accounts |
| WhatsApp | "Don't bulk message, auto-message, or auto-dial using WhatsApp. WhatsApp uses both machine learning technology and reports from users to detect and ban accounts that send unwanted automated messages." Its best practice: "Only send messages to those who have contacted you first or have requested you contact them on WhatsApp." The research's reading, not a WhatsApp statement: an assistant that drafts and sends replies sits near the "auto-message" wording. Anecdotal reports, none from WhatsApp, include bans of accounts that only replied to incoming messages at low volume | WhatsApp Help Center, using WhatsApp responsibly; the library issue trackers |

**What the design does.** Some of it follows what the platforms or the library maintainers say; some of it is a
precaution whose effect is unproven. None of it brings the risk to zero, and no source shows how much it lowers it.

- **One persistent connection set per account.** One sidecar process per account, which never opens a second
  connection set: Messenger holds its main socket plus, once the encrypted-chat device is registered, one more
  for encrypted chats; WhatsApp holds one. Each sidecar locks its data folder and refuses a second process on it. A
  second process on the same WhatsApp keys would make WhatsApp end the first (`stream_replaced`), which can look like
  abuse.
- **WhatsApp presence stays "unavailable".** The sidecar never marks the device available, never subscribes to
  anyone's presence and never sends a read receipt on its own: blue ticks go out only for an explicit
  `whatsapp_personal_mark_read`. The reason is notification routing (marking a companion available may change when
  your phone buzzes), not a known ban trigger, and whether staying unavailable makes the Linked devices screen show the
  device as stale for the 30-day inactivity rule is **UNVERIFIED** (Part 10.3).
- **No new chats, no bulk operations.** No operation creates a group or a broadcast, or edits or unsends a message.
  Each sidecar sends only into a conversation it already holds: the WhatsApp sidecar into a chat that is in its
  store, the Messenger sidecar into a thread that is in its thread list (Part 10.4).
- **A human can approve each send, through your MCP client.** The personal send tools are marked as not read-only
  and their descriptions say they reach real people as you and to confirm intent first. meta-mcp has no approval
  step of its own: whether your client asks you depends on its settings.
- **Nothing paces sends.** The daemon and the sidecars do not limit the rate of sends, reactions, read marks or typing.
  The figures that circulate (at least 500 ms between messages, about 20 per minute per account) are folklore:
  neither is a threshold WhatsApp published.
- **A fatal signal stops sending** until you act (Part 10.5).
- **Read-only mode** (Part 10.4) removes every tool that sends.

**The honest statement.** The risk is not zero, and the account at risk is your phone number or your Facebook
identity. If WhatsApp bans it, you lose access to your chat history and backups on the phone while the ban
lasts, not just the assistant. The Messenger sidecar's documentation says to use an account you can afford to
lose. A separate number limits the damage to that number, but it is then not your personal account.

**Before you start.**

1. Decide that you accept the risk above for the account you are about to use.
2. Turn on two-factor authentication on Facebook, which the mautrix-meta documentation recommends.
3. Think about the people in your chats. Everything the assistant reads goes to whichever language-model
   provider your client uses, and your contacts did not agree to that.
4. Plan to open WhatsApp on your phone at least every 14 days (10.3).
5. Treat the files meta-mcp keeps like passwords (10.5 and [SECURITY.md](../SECURITY.md#session-files)).

**Platform and licence.** Personal mode needs Linux. The daemon speaks over a Unix socket and reads its own user id
from `/proc`, and `personal install` writes systemd user units. Only Linux was exercised. The personal module uses
Unix-only APIs without a platform guard, so the crate is not expected to build on Windows (not tried; the older modules keep fallbacks for non-Unix systems,
this one does not). The
Messenger sidecar links mautrix-meta, which is AGPL-3.0, so the sidecar documentation states that the combined
`meta-messenger-sidecar` binary is AGPL-3.0 and that distributing it carries the AGPL source obligations. Both
sidecars also link `go.mau.fi/libsignal` (the Messenger sidecar through mautrix-meta and `whatsmeow`), which is
GPL-3.0, and `whatsmeow` itself is MPL-2.0.
The `meta-mcp` binary stays MIT. Not legal advice.

### 10.1 Install

1. **What you need.** Rust 1.89 or newer and a C compiler (Part 4.1); Go 1.26 or newer, because the sidecars' `go.mod`
   says `go 1.26.0` and their build script uses the local toolchain (`GOTOOLCHAIN=local`); `govulncheck` (for
   example `go install golang.org/x/vuln/cmd/govulncheck@latest`) and `file`, which the build script runs; and,
   for `personal install`, a systemd user session.

2. **Build all three programs.** From the repository root:

   ```bash
   scripts/build-all.sh
   ```

   It runs `cargo build --release --locked`, then each sidecar's `build.sh` (`go vet`, `go test`, a static
   `CGO_ENABLED=0` build, `govulncheck` on the source and on the binary; any failure stops it), and copies
   `meta-mcp`, `meta-messenger-sidecar` and `meta-whatsapp-sidecar` into `dist/` (`DIST=...` picks another folder).
   `cargo install --path .` installs only `meta-mcp`: it does not build the sidecars.

3. **Put the three files together.** meta-mcp finds the sidecars **next to its own executable** (the real file:
   symlinks are followed), or in `META_MCP_SIDECAR_DIR` when that is set. The names must stay as they are.

   ```bash
   install -m 0755 dist/meta-mcp dist/meta-messenger-sidecar dist/meta-whatsapp-sidecar ~/.local/bin/
   ```

   Or keep them in `dist/` and export `META_MCP_SIDECAR_DIR` with its absolute path.

4. **Look before you install.** `--dry-run` prints the two units and the commands, and writes and runs nothing:

   ```bash
   meta-mcp personal install --dry-run
   ```

5. **Install and start the daemon** as systemd user units:

   ```bash
   meta-mcp personal install
   meta-mcp personal status
   ```

   `install` refuses to run (and says which file it could not find) when a sidecar binary cannot be located. It
   writes `meta-mcp-personal.socket` and `meta-mcp-personal.service` into `$XDG_CONFIG_HOME/systemd/user`
   (else `~/.config/systemd/user`), runs `systemctl --user daemon-reload` and
   `systemctl --user enable --now meta-mcp-personal.socket meta-mcp-personal.service`, and ends with
   "The daemon runs from login". Running it again rewrites the units and reloads systemd, but `enable --now` does not
   restart a service that is already running: restart it (`systemctl --user restart meta-mcp-personal.service`) to
   pick up a new binary path or new `Environment=` lines.

   ```ini
   # meta-mcp-personal.socket
   [Unit]
   Description=meta-mcp personal daemon socket

   [Socket]
   ListenStream=%t/meta-mcp/personal.sock
   SocketMode=0600
   DirectoryMode=0700
   RemoveOnStop=yes

   [Install]
   WantedBy=sockets.target
   ```

   ```ini
   # meta-mcp-personal.service
   [Unit]
   Description=meta-mcp personal daemon (Messenger and WhatsApp)
   Requires=meta-mcp-personal.socket
   After=meta-mcp-personal.socket

   [Service]
   Type=simple
   ExecStart=/home/USER/.local/bin/meta-mcp personal daemon
   Environment="META_MCP_HOME=/home/USER/.config/meta-mcp"
   Restart=on-failure
   RestartSec=5
   RestartPreventExitStatus=2
   UMask=0077
   NoNewPrivileges=yes
   TimeoutStopSec=20

   [Install]
   WantedBy=default.target
   ```

   - `ExecStart` is the canonical path of the binary that ran `install`. If you move the binary, run `install`
     again.
   - **The service does not inherit your shell.** `install` copies `META_MCP_HOME` (always) and, when your shell
     sets them, `META_MCP_INBOX_DB`, `META_MCP_FILES_DIR`, `META_MCP_SIDECAR_DIR` (paths are made absolute),
     `META_MCP_READ_ONLY` and `META_MCP_LOG` into `Environment=` lines. Set them **before** you run `install`, or
     edit the unit and run `systemctl --user daemon-reload` and `systemctl --user restart
     meta-mcp-personal.service`. Nothing else is copied (`META_MCP_MAX_FILE_BYTES` and `META_MCP_PERSONAL_BACKOFF_MS` are
     not, so set those in the unit yourself): no Meta token is needed.
   - A systemd user service starts when you log in and stops with your last session. To keep the daemon up
     without a login, enable lingering: `loginctl enable-linger`.
   - The daemon logs to stderr, so under systemd it is in the journal:
     `journalctl --user -u meta-mcp-personal.service`.
   - `RestartPreventExitStatus=2` means a configuration error (exit status 2) is not retried in a loop.
   - **To stop the daemon, stop both units:** `systemctl --user stop meta-mcp-personal.service
     meta-mcp-personal.socket`. The socket unit stays active when only the service is stopped, and the next client
     connection (any `meta-mcp personal` command or personal tool) starts the daemon again.

6. **Without systemd.** Run the daemon yourself, under any supervisor, with the environment you want:

   ```bash
   meta-mcp personal daemon
   ```

   It listens on the default socket (below) or on `--socket PATH`, runs until SIGTERM or SIGINT, and removes the
   socket it bound when it stops. **There can be only one daemon per `META_MCP_HOME`:** it takes an exclusive lock on
   `personal/daemon.lock`, and a second one exits with `another personal daemon already uses ...`.

7. **Tell the assistant's `meta-mcp` where things are.** The assistant starts its own `meta-mcp` (stdio), which
   talks to the daemon over the socket. The two processes must agree on four things:

   | Setting | In the daemon | In the assistant's `meta-mcp` and in `meta-mcp personal ...` |
   |---|---|---|
   | Socket | `ListenStream` of the socket unit (from `META_MCP_PERSONAL_SOCKET` at install time, else `%t/meta-mcp/personal.sock`), or `--socket PATH` | `META_MCP_PERSONAL_SOCKET`, else `$XDG_RUNTIME_DIR/meta-mcp/personal.sock`, else `META_MCP_HOME/personal/personal.sock` |
   | Inbox | `META_MCP_INBOX_DB` | The same file, so `inbox_list` and `inbox_wait` see what the daemon stores |
   | Files directory | `META_MCP_FILES_DIR` (downloads and media sends need it) | The same directory (inline pictures are read by this process) |
   | Read-only | `META_MCP_READ_ONLY` | The same variable (Part 10.4) |

   - **Keep the files directory outside `META_MCP_HOME`.** Startup refuses a `META_MCP_FILES_DIR` that contains the home,
     but accepts one inside it, and the file tools can read everything in the files directory.
   - **The commands and the server have no `--socket` flag.** If the daemon listens somewhere unusual, export
     `META_MCP_PERSONAL_SOCKET` for them too. `personal install` accepts that variable only as an absolute path
     without whitespace.
   - **The path must be short.** A Unix socket path is limited by the operating system to about 100 bytes. A longer
     one stops the daemon with `error: cannot listen: path must be shorter than SUN_LEN`. Use a short
     `META_MCP_PERSONAL_SOCKET`.
   - **A client started without `XDG_RUNTIME_DIR`** falls back to `META_MCP_HOME/personal/personal.sock`, which is
     not where systemd put the socket. Set `META_MCP_PERSONAL_SOCKET` in that client's environment.
   - **`meta-mcp http` serves the personal tools too.** It registers the same tools as the stdio server, so when a
     daemon runs under the same user and the HTTP server finds its socket, every holder of one of the server's API keys
     can read your chats and send as you. Use stdio for the assistant that should have personal mode, or start the HTTP
     server read-only and give its keys only to people you would give the session files.

8. **Connect your assistant.** Personal mode is part of the normal MCP server, so the command is the one from Part 4.8:

   ```bash
   claude mcp add meta-mcp -- meta-mcp
   claude mcp add --env META_MCP_FILES_DIR=/var/lib/meta-mcp/files --env META_MCP_READ_ONLY=true --transport stdio meta-mcp -- meta-mcp
   ```

   Without `meta-mcp setup` the business tools simply report that no Page or number is configured; the 18 personal
   tools work. Their tool descriptions are in [TOOLS.md](TOOLS.md#personal-tools).

9. **Update or remove.** After a new build, replace the three files; `meta-mcp personal restart messenger` or `whatsapp`
   starts a fresh sidecar from the file on disk (for a platform that has a stored session), and
   `systemctl --user restart meta-mcp-personal.service` restarts the daemon. `meta-mcp personal uninstall` (`--dry-run` to preview) stops the daemon and removes the units; the
   stored sessions, the state and the audit log stay under `META_MCP_HOME/personal/`.

### 10.2 Messenger login

The Messenger sidecar logs in the way a browser stays logged in: with the **cookies of a Facebook session** that
you copy into one command. The daemon starts the Messenger sidecar for the login if it is not already running.

**Why Chrome on Linux.** The library always presents a Chrome-on-Linux user agent with matching client hints.
Cookies taken from another browser contradict that, and the sidecar's documentation says this raises the odds of
a checkpoint, so take them from Chrome on Linux. A fingerprint hypothesis from the mautrix-meta issue tracker says
the same and is **UNVERIFIED** (an AI-assisted comment). A user agent that comes with the login is stored but not
applied.

1. **Log in to Facebook in Chrome on Linux.** The mautrix-meta documentation suggests a private window. Use the
   account you decided on in 10.0, with two-factor authentication on.
2. **Copy the cookies.** Either route works:
   - **DevTools, Application tab.** Open DevTools, then **Application > Cookies > https://www.facebook.com**, and
     copy the values of `c_user`, `xs`, `datr`, `sb` and `fr` into a JSON object (this is the sidecar's own
     instruction). Save it in a file that only you can read:

     ```bash
     umask 077
     cat > cookies.json <<'JSON'
     {"c_user": "...", "xs": "...", "datr": "...", "sb": "...", "fr": "..."}
     JSON
     ```

   - **DevTools, Network tab.** Filter the requests by `graphql`, right-click one, **Copy > Copy as cURL** (on Windows the
     POSIX variant), and save the text in a file (or paste it at the prompt in step 3). The command reads the `Cookie` header
     or `-b`/`--cookie` option, and the browser's user agent when the command has one.

   `c_user`, `xs` and `datr` are required. The command also keeps `sb`, `fr`, `wd` and `presence` when present and
   drops every other cookie. A JSON array of `{"name": ..., "value": ...}` objects (a cookie export) works too.
3. **Log in.**

   ```bash
   meta-mcp personal login messenger --cookies-file cookies.json
   ```

   Without a flag the command prints the sidecar's instructions and asks you to paste the cURL command or the
   JSON, with nothing echoed ("a terminal cuts a pasted line at 4096 characters, so for a longer paste use
   `--cookies-file`"). `--cookies-json JSON` takes the same input as an argument, which is visible in the process
   list and in your shell history: prefer the file or the prompt. The two flags exclude each other.

   When you pass the cookies with a flag, the command checks them before it contacts the daemon:
   `error: missing cookies: c_user, xs, datr` (exit status 2) names what is missing and never prints a value.
   Then the sidecar loads your Messenger page with the cookies to check the account, the daemon writes the session
   state to disk, and the command ends with `Logged in to Messenger as Sam Example (100000000000001)`. A refusal
   prints `Login failed: <code>: <message>` (exit status 1), with the pasted values removed from the message. For a
   dead or invalid session the code is `login_failed`; the specific reason (`token_invalidated`, `user_id_is_zero`,
   `dgw_unauthorized`, `account_mismatch`, `account_disabled`) shows in `meta-mcp personal status`.
4. **Delete `cookies.json`** and **close the browser window without logging out.** Logging out of that Facebook
   session ends it, and the sidecar's cookies die with it.

**What gets stored.** In `META_MCP_HOME/personal/messenger/` (folders 0700, files 0600):

| File | Content |
|---|---|
| `state.json` | The cookies, the user agent, your Facebook ID and a flag for whether the encrypted-chat device is registered |
| `data/e2ee.db` | The encrypted-chat device: keys, Signal sessions, and a cache of encrypted messages as plain JSON (about the newest 2000 per thread) and the media keys and download paths of encrypted attachments. It also records which Facebook account it belongs to and, for unencrypted messages that carry media, the attachment details with the signed download link (about the newest 10000), so a download still works after a restart. A store that belongs to another account is wiped when it is opened. Treat it like a password |
| `data/threads.json` | The map between Messenger threads and the shared thread IDs, with the account it belongs to; no message content |
| `data/sidecar.lock` | Not data: a lock that stops a second sidecar process on the same folder |

**Cookie rotation.** Meta rotates cookies while you are connected. The sidecar sends the new set whenever it
changes and the daemon writes it atomically, so you do not paste again.

**Encrypted chats.** After the first connection the sidecar registers one encrypted-chat device with Meta (a
second, WhatsApp-protocol client inside it). Four things to know:

- **New messages only.** Encrypted chats are read from what arrived since that device existed. History from before
  it is not available, and `has_more` stays false past the stored range. Meta documents that on a new device "chat
  history that was end-to-end encrypted does not appear automatically".
- **The PIN restore is deliberately not implemented.** Restoring older encrypted history needs your Secure Storage
  PIN. The sidecar never asks for it, because reports tie that step to Meta revoking the session (pull request 425
  above; the cause is **UNVERIFIED**). The research also infers from the code that a backup key kept only in Google
  Drive or iCloud could not be restored this way.
- **Whether Meta shows a "new device" notice** in its own apps for the registered device is **UNVERIFIED**. So is
  whether encrypted messages sent while the daemon is stopped are delivered when it comes back.
- Encrypted chats take images only when sending, and no captions (Part 10.4).

**Checkpoints and re-login.** Meta can interrupt a session: a redirect to a checkpoint, a challenge, a consent screen
or a suspended-account page. The sidecar fails closed: it stops, reports the reason (`checkpoint_required`,
`challenge_required`, `consent_required`, `account_suspended`), and the daemon forgets the stored session.
Clear the problem on facebook.com yourself (Meta may ask for a captcha, a phone number or a password reset, as
quoted in 10.0), then log in again with fresh cookies. The same applies when the cookies simply die
(`token_invalidated`, `user_id_is_zero` or `dgw_unauthorized`), when the cookies belong to another account
(`account_mismatch`), or when Meta reports the account itself as disabled (`account_disabled`, Lightspeed error 1357053:
fresh cookies will not help, the account does). Cookie lifetime is undocumented. User reports in the mautrix-meta tracker
range from about a week to several months (**UNVERIFIED**, anecdotes).

### 10.3 WhatsApp login

WhatsApp lets you link another device to your account: the daemon links the machine as a **companion device**
the way WhatsApp Web does, and your phone stays the primary device. Run the command in a terminal, and have the
phone in your hand.

**Decide two things before the first link.** Both travel in the pairing handshake, so they apply to a **first link
only**; to change them later you must `logout` (which normally unlinks the device, see 10.5) and link again.

- `--full-history` asks the phone for a full message history sync. The default is off. How much arrives is the phone's
  decision, a full sync can arrive in pieces over a long stretch and the phone can pause it, so keep the daemon
  connected through the first session. Third-party measurements put the default window at roughly three months and
  a full-sync request at roughly a year (**UNVERIFIED**).
- `--device-name NAME` is the name shown in your phone's Linked devices list (1 to 64 printable characters, default
  `meta-mcp`).

**By QR code** (the default):

```bash
meta-mcp personal login whatsapp
```

1. The command draws a QR code in the terminal, with a caption such as `Scan with WhatsApp: Settings, Linked devices,
   Link a device (code 2, valid 20s)`, and redraws it when a new one arrives. WhatsApp sends about six per
   connection: the first is valid for about 60 seconds and the others for about 20, about 160 seconds in all. The QR
   payload is a secret and is never logged.
2. On the phone, open WhatsApp, then **Linked devices > Link a device** (Android: the menu, then Linked devices; iPhone:
   the Settings tab, then Linked devices), verify your identity if asked (biometric or the phone's PIN) and scan.
   If WhatsApp does not recognise the device it asks "Link device / Don't link".
3. The command ends with `Logged in to WhatsApp as Sam Example (15555550100@s.whatsapp.net)`. The sidecar waits for
   the connection and for the pre-key upload before it says so, because stopping it earlier can lose the login.
4. If nobody scanned in time, the codes run out and the command stops with `The QR codes expired: run the command
   again.` The same message appeared when the connection to WhatsApp never came up (an end-to-end check run without a
   network): run the login again once you have one. The command also gives up after 5 minutes. Ctrl-C cancels it.

**By code**, when you cannot scan a QR code on screen (the phone and the machine are apart, for example):

```bash
meta-mcp personal login whatsapp --pair-code 15555550100
```

`--pair-code PHONE` takes **your own number** in international form: digits only (the command strips `+`, spaces,
dashes, dots and brackets). The command refuses fewer than 6 or more than 15 digits; the sidecar is stricter and
answers `bad_params` ("phone must be 7 to 15 digits in international form without a leading 0") for a number with
fewer than 7 digits or a leading 0. It prints `Pairing code: XXXX-XXXX`. On the phone
tap the "Enter code to link new device" notification, check the location and device name, tap Confirm, verify your
identity and type the 8-character code, or go to **Linked devices > Link a device > Link with phone number
instead**. WhatsApp states no exact expiry; treat the code as valid for about 160 seconds, and as a secret. The
sidecar refuses a pairing whose number differs from the digits you gave, so a code typed on the wrong account never
completes. The command prints `error: login_failed: ...` and then keeps waiting: press Ctrl-C, check the number and run
it again.

**The passkey step.** WhatsApp states: "As an enhanced security measure, WhatsApp may require you to use a
passkey when you link your primary phone to a device using WhatsApp Web." It applies to some accounts, for QR and
code linking alike, so you only learn whether yours is gated at the first link. A headless process cannot produce
the passkey assertion: WebAuthn needs a gesture in a browser tab on the `web.whatsapp.com` origin. So the command
hands you the request and takes the answer back:

1. The phone typically shows "Continue on your other device", and the command prints the sidecar's instructions
   and the `public_key` options as JSON.
2. In a **Chrome-based browser** (on Linux only Chrome-based browsers can create the cross-device passkey QR;
   Firefox supports only local passkeys), open https://web.whatsapp.com, open the developer console and run the
   line below, replacing `<public_key>` with the printed JSON pasted as an object literal:

   ```js
   console.log(JSON.stringify((await navigator.credentials.get({publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(<public_key>)})).toJSON()))
   ```

3. The browser walks you through the passkey prompt (a QR code to scan with the phone, or a local passkey) and
   prints one line of JSON.
4. Paste that line at the command's prompt `Paste the assertion JSON (nothing is shown), then press Enter:`. Invalid
   JSON stops the login (exit status 2) and cancels it.
5. The command then prints `Compare this code with the one on your phone: 123456` and asks `Confirm? [y/N]`. Answer
   `y` and pairing completes as above; anything else cancels it. When `whatsmeow` can skip the hand-off to the phone
   it confirms by itself, and this step does not appear.

Whether an already linked session is affected by the passkey gate is **UNVERIFIED** (third-party reports say the gate
sits at the link handshake).

**What gets stored.** In `META_MCP_HOME/personal/whatsapp/` (folders 0700, files 0600):

| File | Content |
|---|---|
| `state.json` | Account metadata only: your JID (which holds your phone number), LID, display name, pairing time, the history choice |
| `data/session.db` | `whatsmeow`'s store: device identity, Signal sessions and sender keys, pre-keys, app state keys, contacts, LID map. **Private keys are in the clear.** Whoever copies it can act as the linked device until you unlink it on the phone |
| `data/messages.db` | The sidecar's own store of chats, messages (text in the clear and the raw protobuf of each), reactions, receipts and contact names. `whatsmeow` keeps no messages, so this is the only history there is |
| `data/sidecar.lock` | Not data: a lock that stops a second sidecar process on the same folder |

Chats are keyed by JID: groups end in `@g.us`, direct chats use `<digits>@s.whatsapp.net` when WhatsApp has told the
sidecar the mapping and otherwise a `@lid` identifier, and each chat carries the other form as `alt_id` when known.

**The 14-day and 30-day rules, and four devices.** WhatsApp: "Linked devices work without your phone online, but
will log out if your phone is unused for over 14 days." and "We'll also automatically disconnect linked devices after
30 days of inactivity." What counts as inactivity for the 30-day rule is not defined, and whether the sidecar's
"unavailable" presence makes the Linked devices screen show the device as stale is **UNVERIFIED**. The practical
rule is to open WhatsApp on the phone at least every 14 days. Up to four linked devices can be attached to one
phone; the sidecar takes one slot. A phone left unused for more than 14 days logs the sidecar out; the sidecar
documentation expects that to arrive as `device_removed`, but which cause maps to which code is not documented.

**Do not call `logout` casually.** When the sidecar is running and connected it removes the linked device on
WhatsApp's side, and it wipes the sidecar's message store and the leftovers in `session.db`; linking again needs the phone, and WhatsApp may refuse a
re-link right after an unlink for a while (reported, **UNVERIFIED**).

### 10.4 Using it

**The tools at a glance.** Every tool has its parameters, results and errors in
[TOOLS.md](TOOLS.md#personal-tools).

| Purpose | Messenger | WhatsApp |
|---|---|---|
| Check both accounts | `personal_status` | `personal_status` |
| List conversations | `messenger_personal_list_threads` | `whatsapp_personal_list_chats` |
| Read one | `messenger_personal_read_thread` | `whatsapp_personal_read_chat` |
| Wait for a new message | `personal_wait_for_message` | `personal_wait_for_message` |
| Send text | `messenger_personal_send_text` | `whatsapp_personal_send_text` |
| Send an image, video, audio clip or file | `messenger_personal_send_media` | `whatsapp_personal_send_media` |
| React | `messenger_personal_react` | `whatsapp_personal_react` |
| Mark read | `messenger_personal_mark_read` | `whatsapp_personal_mark_read` |
| Typing indicator | `messenger_personal_typing` | `whatsapp_personal_typing` |
| Save an attachment | `messenger_personal_download_attachment` | `whatsapp_personal_download_media` |

**A first conversation, tool by tool.**

1. **Check the wiring.** `personal_status` should show each account `logged_in`, `connection: connected` and
   `sending_blocked: false`.
2. **List.** `messenger_personal_list_threads` and `whatsapp_personal_list_chats` return the newest activity first, with
   a `cursor` for more. The `id` of an entry is the `thread_id` (Messenger) or `chat_id` (WhatsApp) of the other tools.
   An encrypted Messenger conversation joins the list when a message arrives in it, and the WhatsApp list comes from
   the sidecar's own store, so a chat that has been silent since pairing may be missing.
3. **Read.** `messenger_personal_read_thread` and `whatsapp_personal_read_chat` return messages newest first. Page back
   with `before_ts_ms` (pass `(ts + 1) * 1000` of the oldest message you hold and skip ids you already have; the
   times are whole seconds). WhatsApp asks your phone for older history when the store runs out, which needs the
   phone online and is the phone's decision. For an unencrypted Messenger thread that is not cached, the sidecar's
   history request starts at the current time, which its documentation marks as not verified against the live
   server (**UNVERIFIED**).
4. **Wait.** `personal_wait_for_message` returns the next live incoming message, `timed_out`, or `lagged` (events
   were dropped: read `inbox_list`).
5. **Reply.** `messenger_personal_send_text` or `whatsapp_personal_send_text`, with `reply_to` to quote a message.
6. **Files.** `messenger_personal_download_attachment` and `whatsapp_personal_download_media` save an attachment into
   the files directory. `*_send_media` sends a file from it.
7. **Small signals.** `*_react` (an empty emoji removes yours in an unencrypted Messenger thread; in an encrypted thread and on WhatsApp
   that is **UNVERIFIED**),
   `*_mark_read` (the other person sees it) and `*_typing`.

**Reading new messages: three ways.** The daemon writes every message it sees into the inbox (the same SQLite file as
the business webhook events), so the inbox tools work with the platforms `messenger_personal` and `whatsapp_personal`.

| | `personal_wait_for_message` | `inbox_wait` | `inbox_list` |
|---|---|---|---|
| Served from | The daemon, live | The inbox file, polled every 500 ms | The inbox file |
| Returns | The next message that someone else sent | New events of any kind that match, up to 200 | Stored events, newest first, with filters |
| Echoes (your messages from your phone, or sent by the assistant outside encrypted Messenger threads), reactions, receipts | Not returned | Returned (`echo`, `reaction`, `read`, `delivery`) | Stored |
| History (WhatsApp's first sync, on-demand history) | Not returned | Returned as ordinary `message` and `echo` events if a batch arrives during the wait, with old `ts` values | Stored, with `"history": true` in `raw` (`include_raw`) |
| Use it for | Waiting for an answer | Reacting to everything that happens | Catching up after a gap |

Filter with `platform` (`messenger_personal`, `whatsapp_personal`) and `kinds` (`message` for messages from other
people). The history WhatsApp sends after pairing can flood `inbox_wait` with old messages, so prefer
`personal_wait_for_message` to wait. The event `id` of a message or echo is `<platform>:<thread_id>:<message_id>` (reactions and receipts have longer
shapes, in [ARCHITECTURE.md](ARCHITECTURE.md#inbox)), `recipient` is the thread to pass to the other tools, and
`sender` is the author. On a fresh inbox the result may carry the `note` about
a missing webhook: it does not concern personal accounts.

**Sending rules.**

- **Existing conversations.** Each sidecar sends only into a conversation it already holds and otherwise answers
  `thread_not_found`. WhatsApp: a chat that is in the sidecar's store ("the owner must open this chat on the phone
  first"); it appears once a message, a history sync or a group-joined notice for it has reached the sidecar.
  Messenger: a thread that is in the sidecar's thread list, which `messenger_personal_list_threads` extends when it
  runs short (an end-to-end encrypted thread joins it when a message arrives in it); the id of a contact you have no
  conversation with is refused, because a send must never fall back to plaintext. Do not ask the assistant to start
  a conversation.
- **Confirm first.** Every send, reaction, read receipt and typing indicator is in your name. The tools say so and
  ask the client to confirm. Use a client that asks before each call and shows you the arguments, and read them.
  A message in your chats that tells the assistant to send something is text from a stranger, not an instruction.
- **Limits.** Text is 1 to 4000 characters (trimmed; `\r\n` becomes `\n`; no control characters other than newline), a
  caption up to 1024, a reaction an emoji (up to 10 characters) or an empty string. Messenger rejects a `reply_to` of
  the wrong kind for the thread (encrypted or not).
- **`send_unknown`.** If the answer is `send_unknown`, the message may have been sent. **Read the thread before you
  retry.** The audit log has an `unknown` line for it.
- **Pace yourself.** Nothing in meta-mcp slows sends down. Do not send in a loop, do not send the same text to many
  chats, and keep the typing indicator for moments when a reply follows.
- **Audit.** Every text, media and reaction send is written to `audit.jsonl` before it is attempted (10.5).

**Attachments and inline pictures.** The daemon downloads by message and attachment id, never by URL, into
`inbound/messenger_personal/` or `inbound/whatsapp_personal/` of `META_MCP_FILES_DIR` (the daemon's). It needs that
variable: without it the call fails with `no_files_dir`. With `inline: true` a small, well-formed PNG, JPEG, GIF or
WEBP picture of at most 2 MiB also comes back to the model, read by the assistant's own `meta-mcp`, which therefore
needs the same `META_MCP_FILES_DIR`. A file under `inbound/` came from another person, so sending it on needs
`allow_inbound: true`, as in Part 5.1. To send a file, put it in the files directory and pass its path; the daemon
copies it into a private outbox for the send and removes the copy afterwards.

**Read-only mode.** `META_MCP_READ_ONLY=true` removes the 10 personal write tools from the MCP server (Part 8.3). The
daemon checks the same variable for itself and refuses `send_text`, `send_media`, `send_reaction`, `mark_read` and
`set_typing` with `read_only`. They are two switches: set it for the daemon (`personal install` copies it into the
service when your shell has it) and for the assistant's `meta-mcp`. Reading, waiting, downloads, logging in and
storing incoming messages continue.

**What this version does not do.**

- Start chats. Each sidecar sends only into a conversation it already holds (above).
- Group management (create, add, remove, rename, admin changes), polls, status updates, newsletters and channels, and calls
  (a call log entry is read as a message of kind `call`; nothing is placed or answered).
- Edit or unsend your own messages (incoming edits and unsends are applied: `edited` and `unsent`; on WhatsApp and in
  encrypted Messenger threads only when the sender wrote the message, or, for an unsend, a known group admin), send contact
  cards, locations, stickers or link previews on WhatsApp, or send view-once media, mentions or disappearing-message
  timers on Messenger.
- Presence: no subscription to anyone's presence, so no online status and no incoming typing indicators.
- Look up numbers or import an address book. Names come from what the platform delivers.
- Send read receipts on its own, or accept and list Messenger message requests.
- On encrypted Messenger chats: send anything but images, send captions, read history from before the device existed.
- More than one account per platform.

### 10.5 Operations

**Status.** `meta-mcp personal status` prints both accounts (the same data as the `personal_status` tool);
`--json` prints the daemon's answer.

```
Daemon: meta-mcp 0.1.0, pid 4242, read-only no, files dir configured

Messenger
  logged in: yes as Sam Example (100000000000001)
  connection: connected
  sending: ok
  last event: 2026-01-15T10:30:00Z
  sidecar: running, version 0.1.0, library mautrix-meta v0.2610.0

WhatsApp
  logged in: yes as Sam Example (15555550100@s.whatsapp.net)
  connection: fatal (temporary_ban)
  sending: BLOCKED (temporary_ban), run meta-mcp personal restart whatsapp
  last event: 2026-01-15T10:31:09Z
  sidecar: running, version 0.1.0, library whatsmeow v0.0.0-20261017175727-b2b1e71d45e2
  last error: temporary_ban
```

| Line | Meaning |
|---|---|
| `logged in` | Whether a session exists, and as whom |
| `connection` | `stopped` (no sidecar), `starting`, `connecting`, `connected`, `disconnected` or `fatal`, with the sidecar's reason in brackets |
| `sending` | `ok`, or `BLOCKED` with the reason (below) |
| `last event` | When the sidecar last said anything |
| `sidecar` | Running, with the sidecar version and the library it was built on, or `not running` |
| `reconnect` | Seconds until the daemon tries to start a stopped sidecar again |
| `last error` | The last failure as a short code (`sidecar_spawn_failed`, `sidecar_not_ready`, `connect_failed`, `sidecar_exited`, ...) |

Before the first login, and after a daemon restart without a session, a platform shows `connection: stopped` and
`sidecar: not running`: its sidecar starts at the first login, and again at daemon start once a session exists. After
a `logout` or a `logged_out` event the sidecar keeps running: the platform shows `connection: disconnected` with the
reason (for example `logout`) and `sidecar: running`. In the text form a code of 20 or more characters, such as
`sidecar_spawn_failed`, is printed as `[redacted]`, because the command scrubs long token-like runs; `--json` has
the real value.

**Blocked states.** After a fatal signal the sidecar refuses sends, reactions, read receipts and typing until the
daemon restarts it, and the daemon stops reconnecting. These are the WhatsApp reasons (as the sidecar documents them):

| `blocked_reason` | Trigger | What to do |
|---|---|---|
| `temporary_ban` | WhatsApp's temporary-ban event. Codes 101 to 106 mean, in the library's words: too many messages to people who do not have you in their address books (101), too many people blocked you (102), too many groups created with such people (103), the same message sent to too many people (104), too many messages to a broadcast list (106). The event carries an expiry | Wait. Stop sending |
| `reachout_timelock` | WhatsApp's reach-out time-lock, or a send acknowledgement with error 463. It blocks messages to chats the account has not talked to, reportedly for 24 hours to 7 days (**UNVERIFIED**), and reports tie it to a companion logout shortly after | Wait |
| `client_outdated` | WhatsApp says the client is too old (connect failure 405) | Update the sidecar pin (below) |
| `stream_replaced` | Another connection is using the same keys, or WhatsApp rejected the client's user agent (connect failure 409; the sidecar documentation gives no fix for that case) | Find and stop the other process (an old sidecar, a copy of `session.db`) |
| `device_removed` | The device was logged out: unlinked on the phone, the inactivity rules, or a main device that is gone (401, 403); which cause maps to which code is not documented. 403 can be a ban or a phone switch, so this is not proof of a non-ban | Link again (10.3) |
| `banned` | Connect failure 406, which WhatsApp's own web code calls BANNED | Read WhatsApp's account-ban page on the phone; link again only if the account is usable |

Then:

1. Fix the cause. **`meta-mcp personal restart whatsapp` clears the block but not the cause**, so do not restart in
   a loop. A daemon restart clears the block too. On WhatsApp a `logout` clears only the daemon's flag: the
   sidecar keeps refusing `connect` and sends, so run `restart` before you log in again.
2. `restart` stops the sidecar, clears the block and the reconnect backoff, and starts a fresh sidecar with the stored
   session. For `device_removed` and `banned` the session was already deleted, so after `restart` you log in again.
3. `meta-mcp personal login ...` is refused while sending is blocked (`sending_blocked`), so `restart` first.

The Messenger sidecar raises four of the same reasons from its encrypted-chat client: `temporary_ban`,
`stream_replaced`, `client_outdated` and `device_removed` (a removed encrypted-chat device; unlike WhatsApp, the
Messenger session itself stays). Sending is then blocked for the whole account and that client stays stopped until the
next restart: reads and downloads of unencrypted attachments keep working, while a download of an attachment from an
encrypted thread answers `not_connected`. The steps above apply, except that the Messenger session (cookies, `state.json`) was not deleted: after `restart` you
do not need to log in again.

Other Messenger signals: a rate limit is a `fatal` connection with a wait, which the daemon retries with its backoff;
`repeated_failures` (six page loads in a row failed without a verdict, which a network outage can also cause; action
`relogin`) is not a block either: the daemon restarts the sidecar after its backoff, and if it keeps failing while
your network is fine, the session may be dead, so log in again;
and a checkpoint, challenge, consent or suspension is a logout (10.2).

**Reconnects.** If a sidecar exits on its own, the daemon restarts it after 10, 30, 60 and 300 seconds, the last delay
repeating, and resets the delay once it is connected. `status` shows `reconnect: in Ns`. A Messenger sidecar can also
end with status 1 or 2 when its library fails hard (it exits on a malformed protocol step, and a panic in a library
goroutine ends the process); the daemon treats that like any other exit.

**Logout.** `meta-mcp personal logout messenger` or `whatsapp` asks the sidecar (if one is running) to log out, then
**tries** to delete the stored state and empties the daemon's memory of the account, and prints `Logged out of
Messenger: the stored session is cleared.` The command prints that line even when the delete failed (the failure shows
in `meta-mcp personal status` as `last error: state_delete_failed`). If the daemon's request to the sidecar failed, it
adds a line telling you to remove the device or session by hand in the account's linked devices or active sessions.

- **WhatsApp:** when the sidecar is running and connected, the device is removed on WhatsApp's side, so it is
  unlinked on your phone, and the sidecar wipes its message store and the leftovers in `session.db` (a failed wipe is
  reported as `io`). If the sidecar is offline, or its unlink call
  fails, only the local copy is deleted and **the device stays linked on the phone, with no warning**: check the
  Linked devices list. If no sidecar is running, the daemon can only delete its own state file and leaves the data
  folder as it is. Linking again needs the phone, and WhatsApp may refuse a re-link right after an unlink for a
  while (reported, **UNVERIFIED**).
- **Messenger:** a running sidecar deletes its cookies, thread map and encrypted-chat device store, also when its
  session had already ended; a failed deletion is reported as `io` (the command prints it in its "could not be told"
  line) and the files stay. If no sidecar is running, only `state.json` is deleted and `data/e2ee.db` and
  `data/threads.json` stay. The sidecar
  documentation lists no call to Facebook: check your account's active sessions if you want that session gone.

Never log out as a way to stop: stopping the daemon (both units, 10.1, or SIGTERM) leaves the sessions in place
and the WhatsApp device linked.

**Updating the sidecars.** Meta changes its protocols often, and the libraries follow: the sidecar documentation puts
Messenger's changes at roughly monthly, and in 2026 the research recorded breaks that needed library changes in March,
July and September. `whatsmeow` publishes no releases, so it is pinned by a pseudo-version, and WhatsApp-side changes
have broken stale builds before (a `device_removed` right after pairing; a passkey requirement from 2026-06-30 that a
library update handled). Bump the pins at least monthly, and after any `client_outdated`. Treat a mass logout right after
pairing as "the pin is stale".

1. WhatsApp: in `sidecars/whatsapp`, `go get go.mau.fi/whatsmeow@latest`, `go mod tidy`, then `./build.sh`. Re-read the
   library's API changes, which still move.
2. Messenger: in `sidecars/messenger`, bump `go.mau.fi/mautrix-meta` and the `go.mau.fi/whatsmeow` version it needs,
   then `./build.sh`. **Keep the `replace github.com/imroc/req/v3 => github.com/beeper/req/v3 ...` line in `go.mod`:**
   messagix is written against that fork and upstream does not compile against it, and a plain `go get -u` or
   `go mod tidy` run by hand can drop the line. Sends and history are the first things to fail after a Meta-side break.
3. `scripts/build-all.sh`, copy the new files over the old ones, then `meta-mcp personal restart messenger` and
   `restart whatsapp`.

**Logs.** The daemon logs to stderr at `meta_mcp=info` (`META_MCP_LOG` changes the filter): one line per request with
the op and the error code, connection events and sidecar exits. It never logs message text, cookies, session state or
phone numbers. The sidecars' own log lines (info level, fixed) land in the same stream, after an allowlist scrubber:
only a fixed list of fields is written (it includes thread and message ids), and out of what stays the Messenger
sidecar cuts URLs, cookies, tokens and plus-prefixed phone numbers, the WhatsApp sidecar user JIDs, phone numbers, QR
payloads and pair codes ([SECURITY.md](../SECURITY.md#sidecars-logs-and-scrubbing) has the full lists). Under systemd that is
`journalctl --user -u meta-mcp-personal.service -f`. The scrubbers are pattern based, so do not paste raw logs
anywhere public without reading them.

**Backups.** `META_MCP_HOME/personal/` holds your sessions: treat it like a password store. Back it up only to
encrypted storage, or not at all, since you can always log in again. Never copy `session.db` to another machine: two
processes on the same keys make WhatsApp end the first (`stream_replaced`). To move to another machine, log in
there and unlink the old device. What a new link cannot bring back is the history that only the old `messages.db`
held. `inbox.db` is separate (Part 8.8) and holds your conversations too.

**The audit log.** `META_MCP_HOME/personal/audit.jsonl` has two lines for every send: an `attempt`, written and
flushed to disk before the sidecar is asked, and its outcome (`sent`, `failed` or `unknown`). Refused and invalid
requests are logged too. A line never holds the text, only its SHA-256 and length:

```text
{"id":1,"ts":"2026-01-15T10:30:00Z","platform":"whatsapp","thread_id":"15555550100@s.whatsapp.net","kind":"text","body_sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824","body_len":5,"outcome":"attempt"}
{"id":2,"ts":"2026-01-15T10:30:01Z","platform":"whatsapp","thread_id":"15555550100@s.whatsapp.net","kind":"text","body_sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824","body_len":5,"outcome":"sent","attempt_id":1,"message_id":"3EB0A1B2C3D4E5F60718"}
```

`kind` is `text`, `media` or `reaction` (media lines also carry `file_sha256`), `outcome` is `attempt`, `sent`, `failed`,
`unknown`, `invalid` or `refused`, and a completion line has `attempt_id`, `message_id` or `error_code`. Read it with
`tail` or `jq` (`jq -c 'select(.outcome == "unknown")' audit.jsonl` lists the sends whose outcome you must check). If
the daemon cannot write an `attempt` line it sends nothing (`audit_failed`). There is no rotation: to archive the
file, stop the daemon first (both units, 10.1). `mark_read` and `set_typing` are not logged.

### 10.6 Troubleshooting (personal mode)

Read the error text first: a personal tool error is `<code>: <message>`, and the code is the stable part.

| Symptom | Cause | Fix |
|---|---|---|
| `personal mode not running: start it with ... (socket PATH: ...)` | Nothing listens on the socket the client tried (the message gives its path), or the client looks somewhere else | Check `systemctl --user status meta-mcp-personal.socket meta-mcp-personal.service`. Run `meta-mcp personal install` or `meta-mcp personal daemon`. If the path is under `META_MCP_HOME/personal/`, the client had no `XDG_RUNTIME_DIR`: set `META_MCP_PERSONAL_SOCKET` for it (10.1, step 7) |
| `error: cannot listen: path must be shorter than SUN_LEN` | The socket path is longer than the operating system allows (about 100 bytes) | Use a shorter `META_MCP_PERSONAL_SOCKET` or `--socket` |
| `error: another personal daemon already uses ...` or `another personal daemon is already running on ...` | One daemon per `META_MCP_HOME`, and one per socket | Stop the other one (both units, or find the process), then start this one |
| `sidecar_unavailable: could not start the sidecar at PATH: No such file or directory`, or `meta-mcp personal install` saying `the sidecar binaries cannot be located`; `status --json` shows `last_error` `sidecar_spawn_failed` (`sidecar_not_found` only when the daemon cannot work out where to look) | The sidecar binary is not where the daemon looks, or is not executable | Put the three files together (10.1, step 3) or set `META_MCP_SIDECAR_DIR` in the daemon's environment, then `meta-mcp personal restart PLATFORM`. The path in the message is the one the daemon tried |
| `sidecar_unavailable: the sidecar did not report ready` (`last error` `sidecar_not_ready`) or `did not accept hello` (`hello_failed`) | The file is not a sidecar of this version (it must speak protocol 1 and print `ready` within 10 seconds), or another sidecar process already holds the same data folder | Rebuild with `scripts/build-all.sh` and replace all three files; for a second process, find and stop it |
| `error: missing cookies: ...` (exit status 2) | The pasted cookies lack `c_user`, `xs` or `datr` | Copy them again from DevTools (10.2) |
| `Login failed: login_failed ...` (the reason, `token_invalidated`, `user_id_is_zero`, `dgw_unauthorized` or `account_mismatch`, shows in `meta-mcp personal status`) | Meta refused the cookies: they expired, you logged out of that session, or they belong to another account | Log in to Facebook in Chrome, copy fresh cookies, and do not log out of that window |
| `account_disabled` (in `meta-mcp personal status`, after `Login failed: login_failed ...`) | Meta reports the account itself as disabled (Lightspeed error 1357053) | Fresh cookies will not help: resolve it with Meta |
| `checkpoint_required`, `challenge_required`, `consent_required`, `account_suspended` | Meta redirected the session to a checkpoint, challenge, consent or suspended-account page. The mautrix-meta documentation names suspicious activity as one cause, and users report the message "We suspect automated behaviour on your account" (10.0) | Open facebook.com, complete what Meta asks (a captcha, a phone number, a password reset), then log in again with fresh cookies |
| `The QR codes expired: run the command again.` (`qr_timeout`) | Nobody scanned in time, or the connection to WhatsApp never came up (seen with no network) | Run `meta-mcp personal login whatsapp` again, with a network, and scan within the code's lifetime |
| The login prints a passkey request | WhatsApp requires a passkey for this account | Follow the passkey steps in 10.3 in a Chrome-based browser |
| `error: --pair-code needs the phone number in international form, digits only` (exit status 2) | The number is not 6 to 15 digits once `+`, spaces, dashes, dots and brackets are removed | Give your own number with its country code |
| `bad_params: phone must be 7 to 15 digits in international form without a leading 0` | The sidecar refuses a number with fewer than 7 digits or a leading 0 that the command let through | Give your own number with its country code and no leading 0 |
| `error: login_failed ...` on a code login, and the command keeps waiting | The code was typed on an account whose number differs from the digits you gave, or WhatsApp refused | Press Ctrl-C, check the number, and run it again |
| `rate_limited` when asking for a pairing code | WhatsApp rate-limited code requests | Wait, then retry; or link by QR code |
| `sending_blocked: sending is blocked on whatsapp (REASON): ...` (or `messenger`) | A fatal signal (10.5) | Fix the cause, then `meta-mcp personal restart whatsapp` (or `messenger`) |
| `stream_replaced` | Another process uses the same keys (WhatsApp reports a rejected client user agent, 409, the same way, and the sidecar documentation gives no fix for that) | Stop the other process (an old sidecar, a copy of the data folder), then `restart` |
| `client_outdated` | The sidecar's library is too old for WhatsApp (on Messenger, for its encrypted-chat client) | Update the pin and rebuild (10.5) |
| `send_unknown: the message may have been sent: read the thread before retrying` | A timeout or a broken link after the send was written | Read the thread with the read tool. Send again only if the message is not there. The audit log has the `unknown` line |
| `send_failed` | The platform rejected the message (Messenger: "facebook rejected the message") | Read the message text; check the thread is one you can write to |
| `not_logged_in: not logged in to PLATFORM ...` | No session, or the service ended it | `meta-mcp personal login PLATFORM` |
| `read_only: the daemon runs read-only (META_MCP_READ_ONLY)` | The daemon was started read-only | Restart it without the variable (edit the unit, reload, restart) if you want sends |
| `no_files_dir: set META_MCP_FILES_DIR to download attachments` (or `to send files`) | The daemon has no files directory | Set `META_MCP_FILES_DIR` in the daemon's environment, restart it, and point the assistant's `meta-mcp` at the same directory |
| `file_refused: ...` | The path is outside the files directory, is not a file, is over `META_MCP_MAX_FILE_BYTES`, or is under `inbound/` without `allow_inbound` | Fix the path or the flag. The message says which |
| `audit_failed: the audit log could not be written, so nothing was sent` | `personal/audit.jsonl` cannot be appended to (disk full, permissions) | Fix the disk or the permissions, then send again |
| `thread_not_found` | WhatsApp: the chat is not in the sidecar's store. Messenger: a send, reaction, read mark or typing indicator needs a thread in the sidecar's thread list (a bare contact id is refused) | WhatsApp: open the chat on your phone and let a message or history arrive. Messenger: page through `messenger_personal_list_threads` (the `cursor` or a larger `limit`), or let a message arrive in an encrypted thread. Take ids from the list tools or from an event's `recipient` |
| `attachment_expired` | The platform refused the media even after a refresh | WhatsApp needs the phone online to re-upload expired media and does not ask after a fatal signal. Ask for the file to be sent again |
| `too_many_requests`, `too_many_waiters` | More than 32 requests or 16 waits in flight on one connection | Wait for earlier calls to finish |
| `inbox_list` shows no personal events | The daemon and the assistant's `meta-mcp` use different inbox files, or the daemon is not connected | Compare `META_MCP_INBOX_DB` in both (10.1, step 7) and check `meta-mcp personal status` |
| WhatsApp logs out about two weeks after the last time you opened it on the phone | The 14-day rule (it may show as `device_removed`) | Open WhatsApp on the phone at least every 14 days (10.3) |

### 10.7 Command reference and sources

Every personal command, with its flags (`-h` and `--help` print the same on each):

| Command | What it does | Flags and arguments |
|---|---|---|
| `meta-mcp personal install` | Writes and starts the systemd user units | `--dry-run`: print the units and the commands, write and run nothing |
| `meta-mcp personal uninstall` | Stops the daemon and removes the units. Sessions, state and the audit log stay | `--dry-run`: print what would happen |
| `meta-mcp personal daemon` | Runs the daemon in the foreground | `--socket PATH`: the Unix socket to listen on. Default: `META_MCP_PERSONAL_SOCKET`, else `$XDG_RUNTIME_DIR/meta-mcp/personal.sock`, else `META_MCP_HOME/personal/personal.sock` |
| `meta-mcp personal login messenger` | Logs in with browser cookies | `--cookies-file PATH`: a cURL command, a JSON object of cookies or a JSON array of `{name, value}`. `--cookies-json JSON`: the same as a string (visible in the process list). The two exclude each other. With neither, it prompts |
| `meta-mcp personal login whatsapp` | Links the machine as a companion device | `--pair-code PHONE`: link with a code typed on the phone instead of a QR code. `--full-history`: ask the phone for the full history (first link only). `--device-name NAME`: the name in the Linked devices list (default `meta-mcp`) |
| `meta-mcp personal status` | Shows both accounts | `--json`: print the daemon's answer as JSON |
| `meta-mcp personal logout PLATFORM` | Forgets a session | `PLATFORM` is `messenger` or `whatsapp` |
| `meta-mcp personal restart PLATFORM` | Restarts a sidecar and clears a sending block | `PLATFORM` is `messenger` or `whatsapp` |

Exit codes: 0 success, 1 failure (a daemon or login error, or a daemon that is not running), 2 usage error (bad
cookies, bad flag values; the daemon also exits 2 on a configuration error).

**Sources for this part.** Meta and WhatsApp pages as rendered on 2026-10-24, and the documentation of the projects behind the libraries.

- WhatsApp Help Center: linking by QR code (`faq.whatsapp.com/1317564962315842`) and by phone number
  (`faq.whatsapp.com/1324084875126592`), about linked devices (`faq.whatsapp.com/378279804439436`), linking with a passkey
  (`faq.whatsapp.com/1011243695265795`), unofficial apps (`faq.whatsapp.com/1217634902127718`), bans for unofficial apps
  (`faq.whatsapp.com/1064395290901991`), temporary bans (`faq.whatsapp.com/1848531392146538`), account bans
  (`faq.whatsapp.com/465883178708358`), restricted accounts (`faq.whatsapp.com/717472490411581`), and using WhatsApp
  responsibly (`faq.whatsapp.com/361005896189245`).
- Facebook Terms (`facebook.com/terms.php`), the Automated Data Collection Terms
  (`facebook.com/apps/site_scraping_tos_terms.php`) and the Messenger help page on end-to-end encryption
  (`facebook.com/help/messenger-app/1084673321594605`).
- The documentation, code and issue trackers of `go.mau.fi/mautrix-meta` (messagix) and `go.mau.fi/whatsmeow`, and
  `sidecars/messenger/docs/SIDECAR-MESSENGER.md` and `sidecars/whatsapp/docs/SIDECAR-WHATSAPP.md` in this repository.

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
| Messenger (unless `--no-messenger`) | `pages_show_list`, `pages_manage_metadata`, `pages_messaging`, `pages_read_engagement`, `business_management` |
| With WhatsApp | adds `business_management`, `whatsapp_business_messaging`, `whatsapp_business_management`, unless a System User token serves WhatsApp (`--whatsapp-token`, or one already stored and still valid), in which case the login does not ask for them |
| With `--instagram` | adds `pages_show_list`, `instagram_basic`, `instagram_manage_messages` |
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
| **Personal daemon** | `meta-mcp personal daemon`: the long-lived process that owns the personal sidecars, the session files, the send audit log and the Unix socket (Part 10) |
| **Sidecar** | A Go program that the personal daemon starts for one account, Messenger or WhatsApp, and talks to over pipes |
| **Linked device** | A companion device attached to a WhatsApp account, up to four per phone. The WhatsApp sidecar is one |
| **JID, LID** | WhatsApp identifiers. A JID looks like `<digits>@s.whatsapp.net` (a person, by phone number) or `...@g.us` (a group). A LID (`...@lid`) is a hidden-user identifier that WhatsApp uses in place of the number for some chats |
| **Checkpoint** | Facebook's interruption of a session that asks for a captcha, a phone number or a password reset |
